//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Embedded SQL expression and assignment-target lowering.

use super::{
    expect_tag, json_i64_or_zero, require_nonempty_str, Expr, JSONValue, Result, SQLError,
    Statement,
};

pub(super) fn lower_expr_list(raw: Option<&JSONValue>) -> Result<Vec<Expr>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let list = raw
        .as_array()
        .ok_or_else(|| SQLError::Internal("PL/pgSQL expression list is not an array".into()))?;
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        out.push(lower_expr(item)?);
    }
    Ok(out)
}

/// Lower a `PLpgSQL_expr` node whose text is a scalar expression
/// (parse modes 2 = expression, 3/4/5 = assignment source).
pub(super) fn lower_expr(raw: &JSONValue) -> Result<Expr> {
    let (query, mode) = expr_text(raw)?;
    let text = match mode {
        3..=5 => strip_assignment_target(&query, (mode - 2) as usize)?,
        2 => query,
        other => {
            return Err(SQLError::Internal(format!(
                "PL/pgSQL scalar expression has invalid parse mode {other}"
            )));
        }
    };
    compile_expression_text(&text)
}

/// Lower a `PLpgSQL_expr` node holding a complete SQL statement
/// (parse mode 0: queries, PERFORM bodies, CALL statements).
pub(super) fn lower_full_statement(raw: &JSONValue) -> Result<Statement> {
    let (query, mode) = expr_text(raw)?;
    if mode != 0 {
        return Err(SQLError::Internal(format!(
            "embedded PL/pgSQL statement has invalid parse mode {mode}"
        )));
    }
    let mut stmts = crate::compile(&query)?;
    match stmts.len() {
        1 => Ok(stmts.remove(0)),
        n => Err(SQLError::Internal(format!(
            "embedded PL/pgSQL query compiled to {n} statements"
        ))),
    }
}

pub(super) fn expr_text(raw: &JSONValue) -> Result<(String, i64)> {
    let expr = expect_tag(raw, "PLpgSQL_expr", "expression")?;
    let query = require_nonempty_str(expr, "query", "PLpgSQL expression")?;
    // RAW_PARSE_DEFAULT is encoded as zero and therefore omitted by
    // libpg_query's JSON serializer.
    let mode = json_i64_or_zero(expr, "parseMode")?;
    Ok((query, mode))
}

/// Compile a bare expression by wrapping it into `SELECT <expr>`.
pub fn compile_expression_text(text: &str) -> Result<Expr> {
    let stmts = crate::compile(&format!("SELECT {text}"))?;
    let mut stmts = stmts;
    let stmt = match stmts.len() {
        1 => stmts.remove(0),
        n => {
            return Err(SQLError::Parse(format!(
                "expression compiled to {n} statements: {text}"
            )));
        }
    };
    let Statement::Select(select) = stmt else {
        return Err(SQLError::Parse(format!("not an expression: {text}")));
    };
    let mut select = *select;
    if select.projections.len() != 1 || select.from.is_some() {
        return Err(SQLError::Parse(format!("not a single expression: {text}")));
    }
    Ok(select.projections.remove(0).expr)
}

/// Strip the leading `name[.name[.name]] :=` (or `=`) target of an
/// assignment-mode expression, returning the source text.
pub(super) fn strip_assignment_target(text: &str, name_parts: usize) -> Result<String> {
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    let skip_ws = |pos: &mut usize| {
        while *pos < bytes.len() && bytes[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
    };
    for part in 0..name_parts {
        skip_ws(&mut pos);
        if pos < bytes.len() && bytes[pos] == b'"' {
            pos += 1;
            loop {
                if pos >= bytes.len() {
                    return Err(SQLError::Parse(format!(
                        "unterminated quoted identifier in assignment: {text}"
                    )));
                }
                if bytes[pos] == b'"' {
                    if pos + 1 < bytes.len() && bytes[pos + 1] == b'"' {
                        pos += 2;
                        continue;
                    }
                    pos += 1;
                    break;
                }
                pos += 1;
            }
        } else {
            let start = pos;
            while pos < bytes.len()
                && (bytes[pos].is_ascii_alphanumeric()
                    || bytes[pos] == b'_'
                    || bytes[pos] == b'$'
                    || bytes[pos] >= 0x80)
            {
                pos += 1;
            }
            if pos == start {
                return Err(SQLError::Parse(format!(
                    "malformed assignment target: {text}"
                )));
            }
        }
        if part + 1 < name_parts {
            skip_ws(&mut pos);
            if pos >= bytes.len() || bytes[pos] != b'.' {
                return Err(SQLError::Parse(format!(
                    "malformed assignment target: {text}"
                )));
            }
            pos += 1;
        }
    }
    skip_ws(&mut pos);
    if pos < bytes.len() && bytes[pos] == b'[' {
        return Err(SQLError::Unsupported(
            "assignment to an array element".into(),
        ));
    }
    if pos + 1 < bytes.len() && bytes[pos] == b':' && bytes[pos + 1] == b'=' {
        pos += 2;
    } else if pos < bytes.len() && bytes[pos] == b'=' {
        pos += 1;
    } else {
        return Err(SQLError::Parse(format!(
            "assignment operator not found: {text}"
        )));
    }
    Ok(text[pos..].to_string())
}
