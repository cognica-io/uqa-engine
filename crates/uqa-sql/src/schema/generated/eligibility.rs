//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column diagnostics and the SQL type contract of host callbacks.

use crate::SQLError;

pub(crate) fn non_immutable_function() -> SQLError {
    SQLError::Routine {
        sqlstate: "42P17".into(),
        message: "generation expression is not immutable".into(),
    }
}

/// An untyped host result cannot supply a stored generation's static input type. Check original syntax before planning can discard a branch or infer a surrounding cast's result type.
pub(super) fn check_host_return_types(
    expression: &mut crate::ast::Expr,
    is_host_function: impl Fn(&str) -> bool,
) -> Result<(), SQLError> {
    crate::catalog::stored_ast::visit_stored_expression(expression, &mut |node| {
        if let crate::ast::Expr::Func {
            name,
            binding: None,
            ..
        } = node
        {
            if is_host_function(name) {
                return Err(SQLError::TypeMismatch(format!(
                    "registered function `{name}` has no declared SQL return type and cannot be used in a column generation expression"
                )));
            }
        }
        Ok(())
    })
}
