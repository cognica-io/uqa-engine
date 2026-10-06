//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Written SQL function syntax, independent of overload identity.

use uqa_core::Value;

use crate::{ast::Expr, SQLError};

pub(crate) fn extract_fields(args: &[Expr]) -> Result<(&str, &Expr), SQLError> {
    let [field, source] = args else {
        return Err(SQLError::Internal("EXTRACT requires two operands".into()));
    };
    let mut field = field;
    while let Expr::Cast { expr, .. } = field {
        field = expr;
    }
    let (Expr::Literal(Value::Str(field))
    | Expr::TypedLiteral {
        value: Value::Str(field),
        ..
    }) = field
    else {
        return Err(SQLError::Internal(
            "EXTRACT field is not a text constant".into(),
        ));
    };
    Ok((field, source))
}

pub(crate) fn ordinary_function_name(name: &str) -> &str {
    // The unqualified keyword can only have reached a FuncCall as a quoted identifier.
    if name == "extract" {
        "\"extract\""
    } else {
        name
    }
}
