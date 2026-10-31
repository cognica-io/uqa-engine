//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Traverse stored expressions and recognize their column dependencies.

mod walk;

const SUBQUERY_DEPENDENCY_ERROR: &str =
    "schema expression contains a subquery whose dependencies cannot be rewritten safely";

pub fn walk_schema_expr_mut(
    expression: &mut crate::ast::Expr,
    visit: &mut impl FnMut(&mut crate::ast::Expr) -> Result<(), String>,
) -> Result<(), String> {
    walk::walk_mut(expression, visit, &mut || SUBQUERY_DEPENDENCY_ERROR.into())
}

/// Observe the same stored-expression children without copying syntax. Each callback and the subquery diagnostic use the invoking request.
pub fn walk_schema_expr_with_control(
    expression: &crate::ast::Expr,
    control: &uqa_core::memory::ProductionControl<'_>,
    visit: &mut impl FnMut(&crate::ast::Expr) -> Result<(), crate::SQLError>,
) -> Result<(), crate::SQLError> {
    walk::walk(
        expression,
        &mut |node| {
            control.check()?;
            visit(node)
        },
        &mut || match control.copy_text(SUBQUERY_DEPENDENCY_ERROR) {
            Ok(message) => crate::SQLError::Internal(message.into_parts().0),
            Err(error) => error.into(),
        },
    )
}

pub fn schema_expr_references_column(expression: &crate::ast::Expr, column: &str) -> bool {
    references_column(expression, column, true)
}

/// Attribute dependencies survive column renames but disappear with the selected column. A whole-row value depends on its relation and continues to read that relation's current fields after a column is dropped.
pub fn schema_expr_depends_on_column(expression: &crate::ast::Expr, column: &str) -> bool {
    references_column(expression, column, false)
}

fn references_column(expression: &crate::ast::Expr, column: &str, reads_whole_row: bool) -> bool {
    let mut expression = expression.clone();
    let mut referenced = false;
    let result = walk_schema_expr_mut(&mut expression, &mut |node| {
        referenced |= match node {
            crate::ast::Expr::Star | crate::ast::Expr::QualifiedStar(_) => reads_whole_row,
            crate::ast::Expr::Column(name)
            | crate::ast::Expr::QualifiedColumn { column: name, .. } => name == column,
            _ => false,
        };
        Ok(())
    });
    result.is_err() || referenced
}

pub mod oid_alias;
pub mod regclass;
pub mod registration;
pub mod rewrites;
