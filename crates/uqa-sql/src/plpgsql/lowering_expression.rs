//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Embedded SQL expression and assignment-target lowering.

use super::{
    expect_tag, json_i64_or_zero, require_nonempty_str, Expr, JSONValue, PLpgSQLCursorArgument,
    Result, SQLError, Statement,
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
    let parse_mode = match mode {
        2 => pg_query::ParseMode::PlPgSqlExpr,
        3 => pg_query::ParseMode::PlPgSqlAssign1,
        4 => pg_query::ParseMode::PlPgSqlAssign2,
        5 => pg_query::ParseMode::PlPgSqlAssign3,
        other => {
            return Err(SQLError::Internal(format!(
                "PL/pgSQL scalar expression has invalid parse mode {other}"
            )));
        }
    };
    let node = parse_one_raw_node(&query, parse_mode)?;
    match (mode, node.node.as_ref()) {
        (2, Some(pg_query::NodeEnum::SelectStmt(select))) => {
            compile_single_select_expression(select, &query)
        }
        (3..=5, Some(pg_query::NodeEnum::PlassignStmt(assign))) => {
            let expected_names = i32::try_from(mode - 2).map_err(|_| {
                SQLError::Internal(format!("invalid PL/pgSQL assignment parse mode {mode}"))
            })?;
            if assign.nnames != expected_names {
                return Err(SQLError::Internal(format!(
                    "PL/pgSQL assignment parser returned {} target names for parse mode {mode}",
                    assign.nnames
                )));
            }
            let value = assign
                .val
                .as_deref()
                .ok_or_else(|| SQLError::Internal("PL/pgSQL assignment has no value".into()))?;
            compile_single_select_expression(value, &query)
        }
        (_, Some(other)) => Err(SQLError::Internal(format!(
            "PL/pgSQL parse mode {mode} returned unexpected node {other:?}"
        ))),
        (_, None) => Err(SQLError::Internal(
            "PL/pgSQL expression parser returned an empty node".into(),
        )),
    }
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

pub(super) fn lower_cursor_arguments(
    raw: Option<&JSONValue>,
) -> Result<Vec<PLpgSQLCursorArgument>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let (query, mode) = expr_text(raw)?;
    if mode != 2 {
        return Err(SQLError::Internal(format!(
            "PL/pgSQL cursor arguments have invalid parse mode {mode}"
        )));
    }
    let node = parse_one_raw_node(&query, pg_query::ParseMode::PlPgSqlExpr)?;
    let Some(pg_query::NodeEnum::SelectStmt(select)) = node.node.as_ref() else {
        return Err(SQLError::Internal(format!(
            "PL/pgSQL cursor arguments did not parse as a SELECT target list: {query}"
        )));
    };
    validate_select_expression_envelope(select, &query)?;
    if select.target_list.is_empty() {
        return Err(SQLError::Parse(format!(
            "PL/pgSQL cursor argument list is empty: {query}"
        )));
    }
    let projections = crate::compiler::compile_pg_projections(&select.target_list)?;
    Ok(projections
        .into_iter()
        .map(|projection| PLpgSQLCursorArgument {
            name: projection.alias.map(|name| name.to_ascii_lowercase()),
            expr: projection.expr,
        })
        .collect())
}

pub(super) fn expr_text(raw: &JSONValue) -> Result<(String, i64)> {
    let expr = expect_tag(raw, "PLpgSQL_expr", "expression")?;
    let query = require_nonempty_str(expr, "query", "PLpgSQL expression")?;
    // RAW_PARSE_DEFAULT is encoded as zero and therefore omitted by
    // libpg_query's JSON serializer.
    let mode = json_i64_or_zero(expr, "parseMode")?;
    Ok((query, mode))
}

/// Compile a bare expression through `PostgreSQL`'s PL/pgSQL expression parser.
pub fn compile_expression_text(text: &str) -> Result<Expr> {
    let node = parse_one_raw_node(text, pg_query::ParseMode::PlPgSqlExpr)?;
    let Some(pg_query::NodeEnum::SelectStmt(select)) = node.node.as_ref() else {
        return Err(SQLError::Parse(format!("not an expression: {text}")));
    };
    compile_single_select_expression(select, text)
}

fn parse_one_raw_node(text: &str, mode: pg_query::ParseMode) -> Result<pg_query::protobuf::Node> {
    let parsed = pg_query::parse_with_mode(text, mode)?;
    let mut statements = parsed.protobuf.stmts;
    if statements.len() != 1 {
        return Err(SQLError::Parse(format!(
            "PL/pgSQL fragment parsed to {} statements: {text}",
            statements.len()
        )));
    }
    statements
        .remove(0)
        .stmt
        .map(|node| *node)
        .ok_or_else(|| SQLError::Internal("PL/pgSQL parser returned an empty statement".into()))
}

fn compile_single_select_expression(
    select: &pg_query::protobuf::SelectStmt,
    text: &str,
) -> Result<Expr> {
    validate_select_expression_envelope(select, text)?;
    if select.target_list.len() != 1 {
        return Err(SQLError::Parse(format!("not a single expression: {text}")));
    }
    let Some(pg_query::NodeEnum::ResTarget(target)) = select.target_list[0].node.as_ref() else {
        return Err(SQLError::Internal(
            "PL/pgSQL expression target is not a ResTarget".into(),
        ));
    };
    let value = target
        .val
        .as_deref()
        .ok_or_else(|| SQLError::Internal("PL/pgSQL expression target has no value".into()))?;
    crate::compiler::compile_pg_expression(value)
}

fn validate_select_expression_envelope(
    select: &pg_query::protobuf::SelectStmt,
    text: &str,
) -> Result<()> {
    if !select.distinct_clause.is_empty()
        || select.into_clause.is_some()
        || !select.from_clause.is_empty()
        || select.where_clause.is_some()
        || !select.group_clause.is_empty()
        || select.group_distinct
        || select.having_clause.is_some()
        || !select.window_clause.is_empty()
        || !select.values_lists.is_empty()
        || !select.sort_clause.is_empty()
        || select.limit_offset.is_some()
        || select.limit_count.is_some()
        || select.limit_option != pg_query::protobuf::LimitOption::Default as i32
        || !select.locking_clause.is_empty()
        || select.with_clause.is_some()
        || select.op != pg_query::protobuf::SetOperation::SetopNone as i32
        || select.all
        || select.larg.is_some()
        || select.rarg.is_some()
    {
        return Err(SQLError::Parse(format!(
            "PL/pgSQL fragment contains non-expression SELECT state: {text}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed_expression_select() -> pg_query::protobuf::SelectStmt {
        let node = parse_one_raw_node("value + 1", pg_query::ParseMode::PlPgSqlExpr).unwrap();
        let Some(pg_query::NodeEnum::SelectStmt(select)) = node.node else {
            panic!("expression mode did not return SelectStmt");
        };
        *select
    }

    #[test]
    fn expression_envelope_rejects_every_non_expression_select_field() {
        let base = parsed_expression_select();
        validate_select_expression_envelope(&base, "value + 1").unwrap();

        let mut malformed = Vec::new();
        let mut select = base.clone();
        select
            .distinct_clause
            .push(pg_query::protobuf::Node::default());
        malformed.push(select);
        let mut select = base.clone();
        select.into_clause = Some(Box::default());
        malformed.push(select);
        let mut select = base.clone();
        select.group_distinct = true;
        malformed.push(select);
        let mut select = base.clone();
        select.limit_option = pg_query::protobuf::LimitOption::WithTies as i32;
        malformed.push(select);
        let mut select = base.clone();
        select.op = pg_query::protobuf::SetOperation::SetopUnion as i32;
        malformed.push(select);
        let mut select = base;
        select.all = true;
        malformed.push(select);

        for select in malformed {
            assert!(matches!(
                validate_select_expression_envelope(&select, "malformed"),
                Err(SQLError::Parse(message))
                    if message.contains("non-expression SELECT state")
            ));
        }
    }
}
