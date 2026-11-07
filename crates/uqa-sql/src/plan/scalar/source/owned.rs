//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transfer native payloads without allocating replacement boxes or copying declarations.

use super::{Expr, Items, Node, Source};
use std::mem::{replace, take};
use uqa_core::Value;

mod fields;
#[cfg(test)]
mod tests;

#[expect(
    clippy::too_many_lines,
    reason = "exhaustive native scalar payload transfer"
)]
pub(super) fn owned(mut expression: Expr) -> Node<'static> {
    match &mut expression {
        Expr::Star => Node::Star,
        Expr::QualifiedStar(value) => Node::QualifiedStar(Source::Owned(take(value))),
        Expr::Default => Node::Default,
        Expr::Column(value) => Node::Column(Source::Owned(take(value))),
        Expr::QualifiedColumn { qualifier, column } => Node::QualifiedColumn {
            qualifier: Source::Owned(take(qualifier)),
            column: Source::Owned(take(column)),
        },
        Expr::InternalColumn(value) => Node::InternalColumn(*value),
        Expr::Literal(value) => Node::Literal(Source::Owned(replace(value, Value::Null))),
        Expr::RetainedLiteral(value) => Node::RetainedLiteral(value.0.clone()),
        Expr::TypedLiteral {
            value,
            ty,
            collation_oid,
            composite_source,
        } => Node::TypedLiteral {
            value: Source::Owned(replace(value, Value::Null)),
            ty: Source::Owned(take(ty)),
            collation_oid: *collation_oid,
            composite_source: composite_source.take().map(Source::Owned),
        },
        Expr::Param(value) => Node::Param(*value),
        Expr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            order_syntax,
            filter,
        } => Node::Func {
            name: Source::Owned(take(name)),
            binding: binding.take().map(Source::Owned),
            args: Items::Owned(take(args).into_iter()),
            distinct: *distinct,
            order_by: Items::Owned(take(order_by).into_iter()),
            order_syntax: *order_syntax,
            filter: filter.take().map(|value| Source::Owned(*value)),
        },
        Expr::Array(value) => Node::Array(Items::Owned(take(value).into_iter())),
        Expr::Row(value) => Node::Row(Items::Owned(take(value).into_iter())),
        Expr::CompositeRow { items, binding } => Node::CompositeRow {
            items: Items::Owned(take(items).into_iter()),
            binding: Source::Owned(fields::row(binding)),
        },
        Expr::Binary { op, lhs, rhs } => Node::Binary {
            op: *op,
            lhs: child(lhs),
            rhs: child(rhs),
        },
        Expr::UnaryMinus(value) => Node::UnaryMinus(child(value)),
        Expr::Not(value) => Node::Not(child(value)),
        Expr::And(value) => Node::And(Items::Owned(take(value).into_iter())),
        Expr::Or(value) => Node::Or(Items::Owned(take(value).into_iter())),
        Expr::IsNull { expr, negated } => Node::IsNull {
            expr: child(expr),
            negated: *negated,
        },
        Expr::Between { expr, low, high } => Node::Between {
            expr: child(expr),
            low: child(low),
            high: child(high),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Node::InList {
            expr: child(expr),
            list: Items::Owned(take(list).into_iter()),
            negated: *negated,
        },
        Expr::WindowCall {
            name,
            args,
            spec,
            filter,
            modifiers,
        } => Node::WindowCall {
            name: Source::Owned(take(name)),
            args: Items::Owned(take(args).into_iter()),
            spec: Source::Owned(fields::window(spec)),
            filter: filter.take().map(|value| Source::Owned(*value)),
            modifiers: *modifiers,
        },
        Expr::Case {
            base,
            when,
            else_branch,
        } => Node::Case {
            base: base.take().map(|value| Source::Owned(*value)),
            when: Items::Owned(take(when).into_iter()),
            else_branch: else_branch.take().map(|value| Source::Owned(*value)),
        },
        Expr::Cast { expr, ty, implicit } => Node::Cast {
            implicit: *implicit,
            expr: child(expr),
            ty: Source::Owned(take(ty)),
        },
        Expr::ScalarSubquery(value) => Node::ScalarSubquery(Source::Owned(fields::query(value))),
        Expr::Exists { body, negated } => Node::Exists {
            body: Source::Owned(fields::query(body)),
            negated: *negated,
        },
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => Node::InSubquery {
            expr: child(expr),
            body: Source::Owned(fields::query(body)),
            negated: *negated,
        },
    }
}

fn child(value: &mut Expr) -> Source<'static, Expr> {
    Source::Owned(replace(value, Expr::Star))
}
