//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Traverse stored expressions and recognize their column dependencies.

pub fn walk_schema_expr_mut(
    expression: &mut crate::ast::Expr,
    visit: &mut impl FnMut(&mut crate::ast::Expr) -> Result<(), String>,
) -> Result<(), String> {
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
                walk_schema_expr_mut(argument, visit)?;
            }
            for order in order_by {
                walk_schema_expr_mut(&mut order.expr, visit)?;
            }
            if let Some(filter) = filter {
                walk_schema_expr_mut(filter, visit)?;
            }
        }
        Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                walk_schema_expr_mut(item, visit)?;
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            walk_schema_expr_mut(lhs, visit)?;
            walk_schema_expr_mut(rhs, visit)?;
        }
        Expr::Not(inner)
        | Expr::UnaryMinus(inner)
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            walk_schema_expr_mut(inner, visit)?;
        }
        Expr::Between { expr, low, high } => {
            walk_schema_expr_mut(expr, visit)?;
            walk_schema_expr_mut(low, visit)?;
            walk_schema_expr_mut(high, visit)?;
        }
        Expr::InList { expr, list, .. } => {
            walk_schema_expr_mut(expr, visit)?;
            for item in list {
                walk_schema_expr_mut(item, visit)?;
            }
        }
        Expr::WindowCall { args, spec, .. } => {
            for argument in args.iter_mut().chain(&mut spec.partition_by) {
                walk_schema_expr_mut(argument, visit)?;
            }
            for order in &mut spec.order_by {
                walk_schema_expr_mut(&mut order.expr, visit)?;
            }
            if let Some(frame) = &mut spec.frame {
                for bound in [&mut frame.start, &mut frame.end] {
                    match bound {
                        FrameBound::Preceding(expression) | FrameBound::Following(expression) => {
                            walk_schema_expr_mut(expression, visit)?;
                        }
                        FrameBound::UnboundedPreceding
                        | FrameBound::UnboundedFollowing
                        | FrameBound::CurrentRow => {}
                    }
                }
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                walk_schema_expr_mut(base, visit)?;
            }
            for (condition, result) in when {
                walk_schema_expr_mut(condition, visit)?;
                walk_schema_expr_mut(result, visit)?;
            }
            if let Some(else_branch) = else_branch {
                walk_schema_expr_mut(else_branch, visit)?;
            }
        }
        Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
            return Err(
                "schema expression contains a subquery whose dependencies cannot be rewritten safely"
                    .into(),
            );
        }
        Expr::Default
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::InternalColumn(_)
        | Expr::Literal(_)
        | Expr::TypedLiteral { .. }
        | Expr::Param(_) => {}
    }
    Ok(())
}

pub fn schema_expr_references_column(expression: &crate::ast::Expr, column: &str) -> bool {
    let mut expression = expression.clone();
    let mut referenced = false;
    let result = walk_schema_expr_mut(&mut expression, &mut |node| {
        referenced |= match node {
            crate::ast::Expr::Star | crate::ast::Expr::QualifiedStar(_) => true,
            crate::ast::Expr::Column(name)
            | crate::ast::Expr::QualifiedColumn { column: name, .. } => name == column,
            _ => false,
        };
        Ok(())
    });
    result.is_err() || referenced
}

pub mod regclass;
pub mod registration;
pub mod rewrites;
