//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `op_error` for an operator no candidate matches: the operator signature with `format_type_be` type names, and the hint `PostgreSQL` gives for one or two operands.

use crate::{ColumnType, SQLError};

/// `operator does not exist` for a binary operator; `None` is an operand of type `unknown`.
#[must_use]
pub fn undefined_binary_operator(
    left: Option<&ColumnType>,
    symbol: &str,
    right: Option<&ColumnType>,
) -> SQLError {
    undefined_binary_operator_named(&operand_name(left), symbol, &operand_name(right))
}

/// [`undefined_binary_operator`] with operand type names already spelled.
#[must_use]
pub fn undefined_binary_operator_named(left: &str, symbol: &str, right: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42883".into(),
        message: format!("operator does not exist: {left} {symbol} {right}"),
        detail: None,
        hint: Some(
            "No operator matches the given name and argument types. You might need to add explicit type casts."
                .into(),
        ),
    }
}

/// `operator does not exist` for a prefix operator.
#[must_use]
pub fn undefined_prefix_operator(symbol: &str, operand: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42883".into(),
        message: format!("operator does not exist: {symbol} {operand}"),
        detail: None,
        hint: Some(
            "No operator matches the given name and argument type. You might need to add an explicit type cast."
                .into(),
        ),
    }
}

fn operand_name(ty: Option<&ColumnType>) -> String {
    ty.map_or_else(|| "unknown".into(), ColumnType::display_name)
}
