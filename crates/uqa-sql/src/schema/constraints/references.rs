//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow the distinct variables of an analyzed CHECK in their original expression order.

use crate::{ast::Expr, SQLError};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckReference<'a> {
    WholeRow,
    Column(&'a str),
}

impl<'a> CheckReference<'a> {
    /// Whole rows have a relation dependency but no attribute name.
    #[must_use]
    pub const fn column_name(self) -> Option<&'a str> {
        match self {
            Self::WholeRow => None,
            Self::Column(name) => Some(name),
        }
    }

    /// CHECK permits tableoid as its only system attribute; user attributes keep the declaration's durable numbers.
    pub fn attribute_number(
        self,
        lookup: impl FnOnce(&str) -> Result<i16, SQLError>,
    ) -> Result<i16, SQLError> {
        match self {
            Self::WholeRow => Ok(0),
            Self::Column("tableoid") => Ok(-6),
            Self::Column(name) => lookup(name),
        }
    }
}

/// Repeated references retain their first position, including the whole-row variable. Neither the syntax nor its values are cloned or evaluated.
#[must_use]
pub fn check_references(expression: &Expr) -> Vec<CheckReference<'_>> {
    let mut references = References::default();
    collect_references(expression, &mut references);
    references.ordered
}

#[derive(Default)]
struct References<'a> {
    seen: BTreeSet<Option<&'a str>>,
    ordered: Vec<CheckReference<'a>>,
}

impl<'a> References<'a> {
    fn insert(&mut self, reference: CheckReference<'a>) {
        if self.seen.insert(reference.column_name()) {
            self.ordered.push(reference);
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one traversal covers every constraint expression child"
)]
fn collect_references<'a>(expression: &'a crate::ast::Expr, output: &mut References<'a>) {
    use crate::ast::{Expr, FrameBound};
    match expression {
        Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => {
            output.insert(CheckReference::Column(name));
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                collect_references(argument, output);
            }
            for order in order_by {
                collect_references(&order.expr, output);
            }
            if let Some(filter) = filter {
                collect_references(filter, output);
            }
        }
        Expr::Array(items)
        | Expr::Row(items)
        | Expr::CompositeRow { items, .. }
        | Expr::And(items)
        | Expr::Or(items) => {
            for item in items {
                collect_references(item, output);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_references(lhs, output);
            collect_references(rhs, output);
        }
        Expr::Not(inner)
        | Expr::UnaryMinus(inner)
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            collect_references(inner, output);
        }
        Expr::Between { expr, low, high } => {
            collect_references(expr, output);
            collect_references(low, output);
            collect_references(high, output);
        }
        Expr::InList { expr, list, .. } => {
            collect_references(expr, output);
            for item in list {
                collect_references(item, output);
            }
        }
        Expr::WindowCall {
            args, spec, filter, ..
        } => {
            for expression in args
                .iter()
                .chain(filter.as_deref())
                .chain(&spec.partition_by)
                .chain(spec.order_by.iter().map(|order| &order.expr))
            {
                collect_references(expression, output);
            }
            for bound in spec
                .frame
                .iter()
                .flat_map(|frame| [&frame.start, &frame.end])
            {
                if let FrameBound::Preceding(expression) | FrameBound::Following(expression) = bound
                {
                    collect_references(expression, output);
                }
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                collect_references(base, output);
            }
            for (condition, result) in when {
                collect_references(condition, output);
                collect_references(result, output);
            }
            if let Some(else_branch) = else_branch {
                collect_references(else_branch, output);
            }
        }
        Expr::InSubquery { expr, .. } => collect_references(expr, output),
        Expr::Star | Expr::QualifiedStar(_) => {
            output.insert(CheckReference::WholeRow);
        }
        Expr::Default
        | Expr::InternalColumn(_)
        | Expr::Literal(_)
        | Expr::TypedLiteral { .. }
        | Expr::RetainedLiteral(_)
        | Expr::Param(_)
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. } => {}
    }
}
