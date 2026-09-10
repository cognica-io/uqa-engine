//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expression traversal shared by stored statement dependency binding.

use super::{BTreeSet, Expr, ExpressionCallback, SQLError, Statement, StoredAstVisitor};

pub fn visit_stored_expression(
    expression: &mut Expr,
    visit: ExpressionCallback<'_>,
) -> Result<(), SQLError> {
    StoredAstVisitor {
        source: None,
        merge: None,
        expression: Some(visit),
        ty: None,
        relation: &mut |_: &mut String| Ok(()),
        routine: &mut |_: &mut String, _: Option<&mut Option<crate::ast::FunctionBinding>>| Ok(()),
    }
    .bind_expr(expression, &BTreeSet::new())
}

pub fn visit_stored_statement_expressions(
    statement: &mut Statement,
    visit: ExpressionCallback<'_>,
) -> Result<(), SQLError> {
    StoredAstVisitor {
        source: None,
        merge: None,
        expression: Some(visit),
        ty: None,
        relation: &mut |_: &mut String| Ok(()),
        routine: &mut |_: &mut String, _: Option<&mut Option<crate::ast::FunctionBinding>>| Ok(()),
    }
    .bind_statement(statement)
}
