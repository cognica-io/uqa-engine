//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed observation and mutable rewrites share one ordered stored-expression traversal.

macro_rules! schema_expression_walk {
    ($name:ident, $iter:ident, $deref:ident, [$($mutable:tt)*]) => {
        #[expect(
            clippy::too_many_lines,
            reason = "one traversal covers every stored expression child"
        )]
        pub(super) fn $name<E>(
            expression: & $($mutable)* crate::ast::Expr,
            visit: &mut impl FnMut(& $($mutable)* crate::ast::Expr) -> Result<(), E>,
            subquery_error: &mut impl FnMut() -> E,
        ) -> Result<(), E> {
            use crate::ast::{Expr, FrameBound};

            visit(expression)?;
            match expression {
                Expr::Func {
                    args,
                    order_by,
                    filter,
                    ..
                } => {
                    for argument in args {
                        $name(argument, visit, subquery_error)?;
                    }
                    for order in order_by {
                        $name(& $($mutable)* order.expr, visit, subquery_error)?;
                    }
                    if let Some(filter) = filter {
                        $name(filter, visit, subquery_error)?;
                    }
                }
                Expr::Array(items)
                | Expr::Row(items)
                | Expr::CompositeRow { items, .. }
                | Expr::And(items)
                | Expr::Or(items) => {
                    for item in items {
                        $name(item, visit, subquery_error)?;
                    }
                }
                Expr::Binary { lhs, rhs, .. } => {
                    $name(lhs, visit, subquery_error)?;
                    $name(rhs, visit, subquery_error)?;
                }
                Expr::Not(inner)
                | Expr::UnaryMinus(inner)
                | Expr::IsNull { expr: inner, .. }
                | Expr::Cast { expr: inner, .. } => {
                    $name(inner, visit, subquery_error)?;
                }
                Expr::Between { expr, low, high } => {
                    $name(expr, visit, subquery_error)?;
                    $name(low, visit, subquery_error)?;
                    $name(high, visit, subquery_error)?;
                }
                Expr::InList { expr, list, .. } => {
                    $name(expr, visit, subquery_error)?;
                    for item in list {
                        $name(item, visit, subquery_error)?;
                    }
                }
                Expr::WindowCall {
                    args, spec, filter, ..
                } => {
                    for expression in args
                        .$iter()
                        .chain(filter.$deref())
                        .chain(& $($mutable)* spec.partition_by)
                        .chain(spec.order_by.$iter().map(|order| & $($mutable)* order.expr))
                    {
                        $name(expression, visit, subquery_error)?;
                    }
                    for bound in spec
                        .frame
                        .$iter()
                        .flat_map(|frame| [& $($mutable)* frame.start, & $($mutable)* frame.end])
                    {
                        if let FrameBound::Preceding(expression) | FrameBound::Following(expression) = bound
                        {
                            $name(expression, visit, subquery_error)?;
                        }
                    }
                }
                Expr::Case {
                    base,
                    when,
                    else_branch,
                } => {
                    if let Some(base) = base {
                        $name(base, visit, subquery_error)?;
                    }
                    for (condition, result) in when {
                        $name(condition, visit, subquery_error)?;
                        $name(result, visit, subquery_error)?;
                    }
                    if let Some(else_branch) = else_branch {
                        $name(else_branch, visit, subquery_error)?;
                    }
                }
                Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
                    return Err(subquery_error());
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
                | Expr::Param(_) => {}
            }
            Ok(())
        }
    };
}

schema_expression_walk!(walk_mut, iter_mut, as_deref_mut, [mut]);
schema_expression_walk!(walk, iter, as_deref, []);
