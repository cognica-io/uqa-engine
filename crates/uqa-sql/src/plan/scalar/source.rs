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
    QualifiedStar(Source<'a, String>),
    Default,
    Column(Source<'a, String>),
    QualifiedColumn {
        qualifier: Source<'a, String>,
        column: Source<'a, String>,
    },
    InternalColumn(InternalColumnRef),
    Literal(Source<'a, Value>),
    TypedLiteral {
        value: Source<'a, Value>,
        ty: Source<'a, String>,
    },
    Param(usize),
    Func {
        name: Source<'a, String>,
        binding: Option<Source<'a, FunctionBinding>>,
        args: Items<'a, Expr>,
        distinct: bool,
        order_by: Items<'a, OrderBy>,
        filter: Option<Source<'a, Box<Expr>>>,
    },
    Array(Items<'a, Expr>),
    Row(Items<'a, Expr>),
    Binary {
        op: BinaryOp,
        lhs: Source<'a, Box<Expr>>,
        rhs: Source<'a, Box<Expr>>,
    },
    UnaryMinus(Source<'a, Box<Expr>>),
    Not(Source<'a, Box<Expr>>),
    And(Items<'a, Expr>),
    Or(Items<'a, Expr>),
    IsNull {
        expr: Source<'a, Box<Expr>>,
        negated: bool,
    },
    Between {
        expr: Source<'a, Box<Expr>>,
        low: Source<'a, Box<Expr>>,
        high: Source<'a, Box<Expr>>,
    },
    InList {
        expr: Source<'a, Box<Expr>>,
        list: Items<'a, Expr>,
        negated: bool,
    },
    WindowCall {
        name: Source<'a, String>,
        args: Items<'a, Expr>,
        spec: Source<'a, WindowSpec>,
    },
    Case {
        base: Option<Source<'a, Box<Expr>>>,
        when: Items<'a, (Expr, Expr)>,
        else_branch: Option<Source<'a, Box<Expr>>>,
    },
    Cast {
        expr: Source<'a, Box<Expr>>,
        ty: Source<'a, String>,
    },
    ScalarSubquery(Source<'a, Box<SelectStmt>>),
    Exists {
        body: Source<'a, Box<SelectStmt>>,
        negated: bool,
    },
    InSubquery {
        expr: Source<'a, Box<Expr>>,
        body: Source<'a, Box<SelectStmt>>,
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

#[expect(
    clippy::too_many_lines,
    reason = "exhaustive AST projection preserves every scalar variant"
)]
fn owned(expression: Expr) -> Node<'static> {
    match expression {
        Expr::Star => Node::Star,
        Expr::QualifiedStar(value) => Node::QualifiedStar(Source::Owned(value)),
        Expr::Default => Node::Default,
        Expr::Column(value) => Node::Column(Source::Owned(value)),
        Expr::QualifiedColumn { qualifier, column } => Node::QualifiedColumn {
            qualifier: Source::Owned(qualifier),
            column: Source::Owned(column),
        },
        Expr::InternalColumn(value) => Node::InternalColumn(value),
        Expr::Literal(value) => Node::Literal(Source::Owned(value)),
        Expr::TypedLiteral { value, ty } => Node::TypedLiteral {
            value: Source::Owned(value),
            ty: Source::Owned(ty),
        },
        Expr::Param(value) => Node::Param(value),
        Expr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => Node::Func {
            name: Source::Owned(name),
            binding: binding.map(Source::Owned),
            args: Items::Owned(args.into_iter()),
            distinct,
            order_by: Items::Owned(order_by.into_iter()),
            filter: filter.map(Source::Owned),
        },
        Expr::Array(value) => Node::Array(Items::Owned(value.into_iter())),
        Expr::Row(value) => Node::Row(Items::Owned(value.into_iter())),
        Expr::Binary { op, lhs, rhs } => Node::Binary {
            op,
            lhs: Source::Owned(lhs),
            rhs: Source::Owned(rhs),
        },
        Expr::UnaryMinus(value) => Node::UnaryMinus(Source::Owned(value)),
        Expr::Not(value) => Node::Not(Source::Owned(value)),
        Expr::And(value) => Node::And(Items::Owned(value.into_iter())),
        Expr::Or(value) => Node::Or(Items::Owned(value.into_iter())),
        Expr::IsNull { expr, negated } => Node::IsNull {
            expr: Source::Owned(expr),
            negated,
        },
        Expr::Between { expr, low, high } => Node::Between {
            expr: Source::Owned(expr),
            low: Source::Owned(low),
            high: Source::Owned(high),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Node::InList {
            expr: Source::Owned(expr),
            list: Items::Owned(list.into_iter()),
            negated,
        },
        Expr::WindowCall { name, args, spec } => Node::WindowCall {
            name: Source::Owned(name),
            args: Items::Owned(args.into_iter()),
            spec: Source::Owned(spec),
        },
        Expr::Case {
            base,
            when,
            else_branch,
        } => Node::Case {
            base: base.map(Source::Owned),
            when: Items::Owned(when.into_iter()),
            else_branch: else_branch.map(Source::Owned),
        },
        Expr::Cast { expr, ty } => Node::Cast {
            expr: Source::Owned(expr),
            ty: Source::Owned(ty),
        },
        Expr::ScalarSubquery(value) => Node::ScalarSubquery(Source::Owned(value)),
        Expr::Exists { body, negated } => Node::Exists {
            body: Source::Owned(body),
            negated,
        },
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => Node::InSubquery {
            expr: Source::Owned(expr),
            body: Source::Owned(body),
            negated,
        },
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "exhaustive AST projection preserves every scalar variant"
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
        Expr::TypedLiteral { value, ty } => Node::TypedLiteral {
            value: Source::Borrowed(value),
            ty: Source::Borrowed(ty),
        },
        Expr::Param(value) => Node::Param(*value),
        Expr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => Node::Func {
            name: Source::Borrowed(name),
            binding: binding.as_ref().map(Source::Borrowed),
            args: Items::Borrowed(args.iter()),
            distinct: *distinct,
            order_by: Items::Borrowed(order_by.iter()),
            filter: filter.as_ref().map(Source::Borrowed),
        },
        Expr::Array(value) => Node::Array(Items::Borrowed(value.iter())),
        Expr::Row(value) => Node::Row(Items::Borrowed(value.iter())),
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
        Expr::WindowCall { name, args, spec } => Node::WindowCall {
            name: Source::Borrowed(name),
            args: Items::Borrowed(args.iter()),
            spec: Source::Borrowed(spec),
        },
        Expr::Case {
            base,
            when,
            else_branch,
        } => Node::Case {
            base: base.as_ref().map(Source::Borrowed),
            when: Items::Borrowed(when.iter()),
            else_branch: else_branch.as_ref().map(Source::Borrowed),
        },
        Expr::Cast { expr, ty } => Node::Cast {
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
