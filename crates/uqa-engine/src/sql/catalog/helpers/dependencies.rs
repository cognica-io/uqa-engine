//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column dependencies extracted from catalog expressions.

use uqa_sql::ast::{ColumnDef as SQLColumnDef, Expr, WindowFrame, WindowSpec};
use uqa_sql::SQLError;

use super::constraints::ConstraintCatalogColumn;
use super::rows::catalog_ordinal;

pub(super) fn named_constraint_columns(
    names: &[String],
    columns: &[SQLColumnDef],
    table_name: &str,
) -> Result<Vec<ConstraintCatalogColumn>, SQLError> {
    names
        .iter()
        .map(|name| {
            let index = columns
                .iter()
                .position(|column| column.name == *name)
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "constraint on table `{table_name}` references missing column `{name}`"
                    ))
                })?;
            Ok(ConstraintCatalogColumn {
                name: name.clone(),
                table_ordinal: catalog_ordinal(index, "constraint column")?,
            })
        })
        .collect()
}

pub(super) fn check_constraint_columns(
    expression: &Expr,
    columns: &[SQLColumnDef],
    table_name: &str,
) -> Result<Vec<ConstraintCatalogColumn>, SQLError> {
    let mut names = Vec::new();
    collect_expression_columns(expression, &mut names);
    named_constraint_columns(&names, columns, table_name)
}

fn collect_expression_columns(expression: &Expr, output: &mut Vec<String>) {
    match expression {
        Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => {
            if !output.contains(name) {
                output.push(name.clone());
            }
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                collect_expression_columns(argument, output);
            }
            for order in order_by {
                collect_expression_columns(&order.expr, output);
            }
            if let Some(filter) = filter {
                collect_expression_columns(filter, output);
            }
        }
        Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_expression_columns(item, output);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expression_columns(lhs, output);
            collect_expression_columns(rhs, output);
        }
        Expr::Not(inner)
        | Expr::UnaryMinus(inner)
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            collect_expression_columns(inner, output);
        }
        Expr::Between { expr, low, high } => {
            collect_expression_columns(expr, output);
            collect_expression_columns(low, output);
            collect_expression_columns(high, output);
        }
        Expr::InList { expr, list, .. } => {
            collect_expression_columns(expr, output);
            for item in list {
                collect_expression_columns(item, output);
            }
        }
        Expr::WindowCall { args, spec, .. } => {
            for argument in args {
                collect_expression_columns(argument, output);
            }
            collect_window_columns(spec, output);
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                collect_expression_columns(base, output);
            }
            for (condition, result) in when {
                collect_expression_columns(condition, output);
                collect_expression_columns(result, output);
            }
            if let Some(else_branch) = else_branch {
                collect_expression_columns(else_branch, output);
            }
        }
        Expr::InSubquery { expr, .. } => collect_expression_columns(expr, output),
        Expr::Default
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::InternalColumn(_)
        | Expr::Literal(_)
        | Expr::Param(_)
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. } => {}
    }
}

fn collect_window_columns(spec: &WindowSpec, output: &mut Vec<String>) {
    for expression in &spec.partition_by {
        collect_expression_columns(expression, output);
    }
    for order in &spec.order_by {
        collect_expression_columns(&order.expr, output);
    }
    if let Some(frame) = &spec.frame {
        collect_window_frame_columns(frame, output);
    }
}

fn collect_window_frame_columns(frame: &WindowFrame, output: &mut Vec<String>) {
    use uqa_sql::ast::FrameBound;
    for bound in [&frame.start, &frame.end] {
        match bound {
            FrameBound::Preceding(expression) | FrameBound::Following(expression) => {
                collect_expression_columns(expression, output);
            }
            FrameBound::UnboundedPreceding
            | FrameBound::UnboundedFollowing
            | FrameBound::CurrentRow => {}
        }
    }
}
