//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar observations retain ordered child positions across work windows. Query and window bodies remain separate observation scopes.

use crate::{catalog::work::Work, SQLError, ScalarExpr};
use uqa_core::memory::{ProductionControl, ProductionSmallVec};

#[derive(Clone, Copy)]
enum Frame<'a> {
    Visit(&'a ScalarExpr),
    Children(&'a ScalarExpr, usize),
}

pub(crate) enum Decision {
    Descend,
    Prune,
    Stop,
}

pub(crate) async fn visit<'a, W, O>(
    expression: &'a ScalarExpr,
    control: &ProductionControl<'_>,
    work: &mut W,
    include_call_order: bool,
    observe: &mut O,
) -> Result<bool, SQLError>
where
    W: Work,
    O: FnMut(&'a ScalarExpr) -> Result<Decision, SQLError> + ?Sized,
{
    let mut frames = ProductionSmallVec::<[Frame<'_>; 32]>::new(*control);
    frames.push(Frame::Visit(expression))?;
    while let Some(frame) = frames.pop() {
        control.check()?;
        work.checkpoint().await?;
        control.check()?;
        match frame {
            Frame::Visit(expression) => match observe(expression)? {
                Decision::Descend => frames.push(Frame::Children(expression, 0))?,
                Decision::Prune => {}
                Decision::Stop => return Ok(true),
            },
            Frame::Children(expression, index) => {
                if let Some(child) = child(expression, index, include_call_order) {
                    frames.push(Frame::Children(expression, index + 1))?;
                    frames.push(Frame::Visit(child))?;
                }
            }
        }
    }
    Ok(false)
}

fn child(expression: &ScalarExpr, index: usize, include_call_order: bool) -> Option<&ScalarExpr> {
    match expression {
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            if index < args.len() {
                return args.get(index);
            }
            let index = index - args.len();
            let orders = if include_call_order {
                order_by.len()
            } else {
                0
            };
            if index < orders {
                return order_by.get(index).map(|order| &order.expr);
            }
            if index == orders {
                filter.as_deref()
            } else {
                None
            }
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::CompositeRow { items, .. }
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.get(index),
        ScalarExpr::Binary { lhs, rhs, .. } => [lhs.as_ref(), rhs.as_ref()].get(index).copied(),
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::Collated { expr: inner, .. }
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::InSubquery { expr: inner, .. } => (index == 0).then_some(inner.as_ref()),
        ScalarExpr::Between { expr, low, high } => [expr.as_ref(), low.as_ref(), high.as_ref()]
            .get(index)
            .copied(),
        ScalarExpr::InList { expr, list, .. } => {
            if index == 0 {
                Some(expr)
            } else {
                list.get(index - 1)
            }
        }
        ScalarExpr::Case {
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
            if index == 2 * when.len() {
                else_branch.as_deref()
            } else {
                None
            }
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => None,
    }
}
