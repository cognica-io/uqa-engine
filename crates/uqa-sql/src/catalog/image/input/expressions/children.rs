//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One cursor selects native slots in the exact encoded child order.

use super::{invalid, Expr, Frame, Input, SQLError};
use crate::ast::NullsOrder;

impl Frame {
    pub(super) fn prepare_child(&mut self, input: &mut Input<'_>) -> Result<bool, SQLError> {
        input.prepare_expression_child(&mut self.value, self.cursor)
    }

    pub(super) fn child(&mut self) -> Option<&mut Expr> {
        expression_child(&mut self.value, self.cursor)
    }
}

impl Input<'_> {
    pub(in super::super) fn prepare_expression_child(
        &mut self,
        value: &mut Expr,
        cursor: usize,
    ) -> Result<bool, SQLError> {
        if expression_child(value, cursor).is_none() {
            return Ok(false);
        }
        if let Expr::Func { args, order_by, .. } = value {
            if let Some(index) = cursor.checked_sub(args.len()) {
                if let Some(order) = order_by.get_mut(index) {
                    order.descending = self.flag()?;
                    order.nulls = match self.binary.array::<1>()?[0] {
                        0 => None,
                        1 => Some(NullsOrder::First),
                        2 => Some(NullsOrder::Last),
                        _ => return Err(invalid("scalar ordering tag")),
                    };
                }
            }
        }
        Ok(true)
    }
}

pub(in super::super) fn expression_child(value: &mut Expr, cursor: usize) -> Option<&mut Expr> {
    let mut index = cursor;
    match value {
        Expr::Func {
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
        Expr::Array(values)
        | Expr::Row(values)
        | Expr::And(values)
        | Expr::Or(values)
        | Expr::CompositeRow { items: values, .. } => values.get_mut(index),
        Expr::Binary { lhs, rhs, .. } => match index {
            0 => Some(lhs),
            1 => Some(rhs),
            _ => None,
        },
        Expr::UnaryMinus(value)
        | Expr::Not(value)
        | Expr::IsNull { expr: value, .. }
        | Expr::Cast { expr: value, .. } => (index == 0).then_some(value.as_mut()),
        Expr::Between { expr, low, high } => match index {
            0 => Some(expr),
            1 => Some(low),
            2 => Some(high),
            _ => None,
        },
        Expr::InList { expr, list, .. } => {
            if index == 0 {
                Some(expr)
            } else {
                list.get_mut(index - 1)
            }
        }
        Expr::Case {
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
        Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Default
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::InternalColumn(_)
        | Expr::Literal(_)
        | Expr::TypedLiteral { .. }
        | Expr::Param(_)
        | Expr::RetainedLiteral(_) => None,
        Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => unreachable!("queries have no stored scalar tag"),
    }
}
