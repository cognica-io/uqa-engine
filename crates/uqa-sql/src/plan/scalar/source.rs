//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Allocation-free projections let owned and borrowed ASTs share one lowering algorithm.

use crate::ast::{
    BinaryOp, Expr, FunctionBinding, InternalColumnRef, OrderBy, SelectStmt, WindowSpec,
};
use uqa_core::Value;

pub(super) enum Source<'a, T> {
    Owned(T),
    Borrowed(&'a T),
}

impl<'a, T> Source<'a, Box<T>> {
    pub(super) fn unbox(self) -> Source<'a, T> {
        match self {
            Self::Owned(value) => Source::Owned(*value),
            Self::Borrowed(value) => Source::Borrowed(value),
        }
    }
}

pub(super) enum Items<'a, T> {
    Owned(std::vec::IntoIter<T>),
    Borrowed(std::slice::Iter<'a, T>),
}

impl<'a, T> Iterator for Items<'a, T> {
    type Item = Source<'a, T>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Owned(items) => items.next().map(Source::Owned),
            Self::Borrowed(items) => items.next().map(Source::Borrowed),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl<T> ExactSizeIterator for Items<'_, T> {
    fn len(&self) -> usize {
        match self {
            Self::Owned(items) => items.len(),
            Self::Borrowed(items) => items.len(),
        }
    }
}

impl<'a> Source<'a, (Expr, Expr)> {
    pub(super) fn pair(self) -> (Source<'a, Expr>, Source<'a, Expr>) {
        match self {
            Self::Owned((left, right)) => (Source::Owned(left), Source::Owned(right)),
            Self::Borrowed((left, right)) => (Source::Borrowed(left), Source::Borrowed(right)),
        }
    }
}

pub(super) enum Node<'a> {
    Star,
    QualifiedStar(Source<'a, crate::ast::ReferenceName>),
    Default,
    Column(Source<'a, crate::ast::ReferenceName>),
    QualifiedColumn {
        qualifier: Source<'a, String>,
        column: Source<'a, crate::ast::ReferenceName>,
    },
    InternalColumn(InternalColumnRef),
    Literal(Source<'a, Value>),
    RetainedLiteral(crate::ir::RetainedScalarLiteral),
    TypedLiteral {
        value: Source<'a, Value>,
        ty: Source<'a, String>,
        collation_oid: Option<u32>,
        composite_source: Option<Source<'a, Box<crate::expr::composites::CompositeConstantSource>>>,
    },
    Param(usize),
    Func {
        name: Source<'a, String>,
        binding: Option<Source<'a, FunctionBinding>>,
        args: Items<'a, Expr>,
        distinct: bool,
        order_by: Items<'a, OrderBy>,
        order_syntax: crate::ast::FunctionOrderSyntax,
        filter: Option<Source<'a, Expr>>,
    },
    Array(Items<'a, Expr>),
    Row(Items<'a, Expr>),
    CompositeRow {
        items: Items<'a, Expr>,
        binding: Source<'a, crate::ast::CompositeRowBinding>,
    },
    Binary {
        op: BinaryOp,
        lhs: Source<'a, Expr>,
        rhs: Source<'a, Expr>,
    },
    UnaryMinus(Source<'a, Expr>),
    Not(Source<'a, Expr>),
    And(Items<'a, Expr>),
    Or(Items<'a, Expr>),
    IsNull {
        expr: Source<'a, Expr>,
        negated: bool,
    },
    Between {
        expr: Source<'a, Expr>,
        low: Source<'a, Expr>,
        high: Source<'a, Expr>,
    },
    InList {
        expr: Source<'a, Expr>,
        list: Items<'a, Expr>,
        negated: bool,
    },
    WindowCall {
        name: Source<'a, String>,
        args: Items<'a, Expr>,
        spec: Source<'a, WindowSpec>,
        filter: Option<Source<'a, Expr>>,
        modifiers: crate::ast::WindowCallModifiers,
    },
    Case {
        base: Option<Source<'a, Expr>>,
        when: Items<'a, (Expr, Expr)>,
        else_branch: Option<Source<'a, Expr>>,
    },
    Cast {
        implicit: bool,
        expr: Source<'a, Expr>,
        ty: Source<'a, String>,
    },
    ScalarSubquery(Source<'a, SelectStmt>),
    Exists {
        body: Source<'a, SelectStmt>,
        negated: bool,
    },
    InSubquery {
        expr: Source<'a, Expr>,
        body: Source<'a, SelectStmt>,
        negated: bool,
    },
}

impl<'a> Source<'a, Expr> {
    pub(super) fn node(self) -> Node<'a> {
        match self {
            Self::Owned(expression) => owned(expression),
            Self::Borrowed(expression) => borrowed(expression),
        }
    }
}

mod owned;
use owned::owned;

#[expect(
    clippy::too_many_lines,
    reason = "exhaustive AST projection keeps every variant field together"
)]
fn borrowed(expression: &Expr) -> Node<'_> {
    match expression {
        Expr::Star => Node::Star,
        Expr::QualifiedStar(value) => Node::QualifiedStar(Source::Borrowed(value)),
        Expr::Default => Node::Default,
        Expr::Column(value) => Node::Column(Source::Borrowed(value)),
        Expr::QualifiedColumn { qualifier, column } => Node::QualifiedColumn {
            qualifier: Source::Borrowed(qualifier),
            column: Source::Borrowed(column),
        },
        Expr::InternalColumn(value) => Node::InternalColumn(*value),
        Expr::Literal(value) => Node::Literal(Source::Borrowed(value)),
        Expr::RetainedLiteral(value) => Node::RetainedLiteral(value.0.clone()),
        Expr::TypedLiteral {
            value,
            ty,
            collation_oid,
            composite_source,
        } => Node::TypedLiteral {
            value: Source::Borrowed(value),
            ty: Source::Borrowed(ty),
            collation_oid: *collation_oid,
            composite_source: composite_source.as_ref().map(Source::Borrowed),
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
            name: Source::Borrowed(name),
            binding: binding.as_ref().map(Source::Borrowed),
            args: Items::Borrowed(args.iter()),
            distinct: *distinct,
            order_by: Items::Borrowed(order_by.iter()),
            order_syntax: *order_syntax,
            filter: filter.as_deref().map(Source::Borrowed),
        },
        Expr::Array(value) => Node::Array(Items::Borrowed(value.iter())),
        Expr::Row(value) => Node::Row(Items::Borrowed(value.iter())),
        Expr::CompositeRow { items, binding } => Node::CompositeRow {
            items: Items::Borrowed(items.iter()),
            binding: Source::Borrowed(binding),
        },
        Expr::Binary { op, lhs, rhs } => Node::Binary {
            op: *op,
            lhs: Source::Borrowed(lhs),
            rhs: Source::Borrowed(rhs),
        },
        Expr::UnaryMinus(value) => Node::UnaryMinus(Source::Borrowed(value)),
        Expr::Not(value) => Node::Not(Source::Borrowed(value)),
        Expr::And(value) => Node::And(Items::Borrowed(value.iter())),
        Expr::Or(value) => Node::Or(Items::Borrowed(value.iter())),
        Expr::IsNull { expr, negated } => Node::IsNull {
            expr: Source::Borrowed(expr),
            negated: *negated,
        },
        Expr::Between { expr, low, high } => Node::Between {
            expr: Source::Borrowed(expr),
            low: Source::Borrowed(low),
            high: Source::Borrowed(high),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Node::InList {
            expr: Source::Borrowed(expr),
            list: Items::Borrowed(list.iter()),
            negated: *negated,
        },
        window @ Expr::WindowCall { .. } => borrowed_window_call(window),
        Expr::Case {
            base,
            when,
            else_branch,
        } => Node::Case {
            base: base.as_deref().map(Source::Borrowed),
            when: Items::Borrowed(when.iter()),
            else_branch: else_branch.as_deref().map(Source::Borrowed),
        },
        Expr::Cast { expr, ty, implicit } => Node::Cast {
            implicit: *implicit,
            expr: Source::Borrowed(expr),
            ty: Source::Borrowed(ty),
        },
        Expr::ScalarSubquery(value) => Node::ScalarSubquery(Source::Borrowed(value)),
        Expr::Exists { body, negated } => Node::Exists {
            body: Source::Borrowed(body),
            negated: *negated,
        },
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => Node::InSubquery {
            expr: Source::Borrowed(expr),
            body: Source::Borrowed(body),
            negated: *negated,
        },
    }
}

fn borrowed_window_call(window: &Expr) -> Node<'_> {
    let Expr::WindowCall {
        name,
        args,
        spec,
        filter,
        modifiers,
    } = window
    else {
        unreachable!("only window calls reach the window conversion")
    };
    Node::WindowCall {
        name: Source::Borrowed(name),
        args: Items::Borrowed(args.iter()),
        spec: Source::Borrowed(spec),
        filter: filter.as_deref().map(Source::Borrowed),
        modifiers: *modifiers,
    }
}
