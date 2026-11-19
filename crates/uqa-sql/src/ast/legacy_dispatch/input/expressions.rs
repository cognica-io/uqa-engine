//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fixed, Child, Node};
use crate::ast::{Expr, FrameBound, FunctionBinding, WindowSpec};
use uqa_core::{
    image_io::{AsyncImageInput, ImageIoError},
    CancellationToken,
};

pub(super) async fn needs_migration(
    expression: &Expr,
    original: &CancellationToken,
    input: &mut AsyncImageInput<'_>,
) -> Result<bool, ImageIoError> {
    if crate::type_resolution::between::needs_ast_restoration(expression) {
        return Ok(true);
    }
    Ok(
        if let Expr::Func {
            name,
            binding,
            args,
            ..
        } = expression
        {
            FunctionBinding::legacy_serialized_dispatch_from_input(
                name,
                binding.as_ref(),
                original,
                input,
            )
            .await?
            .is_some()
                || FunctionBinding::legacy_array_slice_bounds(
                    binding.as_ref(),
                    args.len(),
                    |index| matches!(args[index], Expr::Literal(uqa_core::Value::Null)),
                )
                .is_some()
        } else {
            false
        },
    )
}

pub(super) fn expression(value: &Expr, position: usize) -> Child<'_> {
    match value {
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => fixed(
            [
                Some(Node::Expressions(args)),
                Some(Node::Orders(order_by)),
                filter.as_deref().map(Node::Expression),
            ],
            position,
        ),
        Expr::Array(values)
        | Expr::Row(values)
        | Expr::CompositeRow { items: values, .. }
        | Expr::And(values)
        | Expr::Or(values) => fixed([Some(Node::Expressions(values))], position),
        Expr::Binary { lhs, rhs, .. } => fixed(
            [Some(Node::Expression(lhs)), Some(Node::Expression(rhs))],
            position,
        ),
        Expr::UnaryMinus(value)
        | Expr::Not(value)
        | Expr::IsNull { expr: value, .. }
        | Expr::Cast { expr: value, .. } => fixed([Some(Node::Expression(value))], position),
        Expr::Between { expr, low, high } => fixed(
            [
                Some(Node::Expression(expr)),
                Some(Node::Expression(low)),
                Some(Node::Expression(high)),
            ],
            position,
        ),
        Expr::InList { expr, list, .. } => fixed(
            [Some(Node::Expression(expr)), Some(Node::Expressions(list))],
            position,
        ),
        Expr::WindowCall {
            args, spec, filter, ..
        } => fixed(
            [
                Some(Node::Expressions(args)),
                filter.as_deref().map(Node::Expression),
                Some(Node::Window(spec)),
            ],
            position,
        ),
        Expr::Case {
            base,
            when,
            else_branch,
        } => fixed(
            [
                base.as_deref().map(Node::Expression),
                Some(Node::Cases(when)),
                else_branch.as_deref().map(Node::Expression),
            ],
            position,
        ),
        Expr::InSubquery { expr, body, .. } => fixed(
            [Some(Node::Expression(expr)), Some(Node::Select(body))],
            position,
        ),
        Expr::ScalarSubquery(body) | Expr::Exists { body, .. } => {
            fixed([Some(Node::Select(body))], position)
        }
        Expr::Default
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::InternalColumn(_)
        | Expr::Literal(_)
        | Expr::TypedLiteral { .. }
        | Expr::RetainedLiteral(_)
        | Expr::Param(_) => Child::Done,
    }
}

pub(super) fn window(value: &WindowSpec, position: usize) -> Child<'_> {
    fn bound(value: &FrameBound) -> Option<Node<'_>> {
        match value {
            FrameBound::Preceding(value) | FrameBound::Following(value) => {
                Some(Node::Expression(value))
            }
            FrameBound::UnboundedPreceding
            | FrameBound::UnboundedFollowing
            | FrameBound::CurrentRow => None,
        }
    }
    fixed(
        [
            Some(Node::Expressions(&value.partition_by)),
            Some(Node::Orders(&value.order_by)),
            value.frame.as_ref().and_then(|frame| bound(&frame.start)),
            value.frame.as_ref().and_then(|frame| bound(&frame.end)),
        ],
        position,
    )
}
