//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Embedded SQL expression and assignment-target lowering.

use super::{
    expect_tag, json_i64_or_zero, require_nonempty_str, Expr, JSONValue, PLpgSQLCompileMode,
    PLpgSQLCursorArgument, PLpgSQLCursorArguments, PLpgSQLExpression, PLpgSQLFragment,
    PLpgSQLParseMode, PLpgSQLSource, PLpgSQLStatement, Result, SQLError, Statement,
};

pub(super) fn lower_expr_list(
    raw: Option<&JSONValue>,
    mode: PLpgSQLCompileMode,
) -> Result<Vec<PLpgSQLExpression>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let list = raw
        .as_array()
        .ok_or_else(|| SQLError::Internal("PL/pgSQL expression list is not an array".into()))?;
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        out.push(lower_expr(item, mode)?);
    }
    Ok(out)
}

/// Lower a `PLpgSQL_expr` node whose text is a scalar expression
/// (parse modes 2 = expression, 3/4/5 = assignment source).
fn source(raw: &JSONValue) -> Result<std::sync::Arc<PLpgSQLSource>> {
    let (query, mode) = expr_text(raw)?;
    Ok(std::sync::Arc::new(PLpgSQLSource {
        query: query.into(),
        mode: mode.try_into()?,
    }))
}

pub(super) fn lower_expr(raw: &JSONValue, mode: PLpgSQLCompileMode) -> Result<PLpgSQLExpression> {
    let source = source(raw)?;
    if source.mode == PLpgSQLParseMode::Statement {
        return Err(SQLError::Internal(
            "PL/pgSQL scalar expression has statement parse mode".into(),
        ));
    }
    let validation = mode
        .validates()
        .then(|| compile_expression_source(&source))
        .transpose()?;
    Ok(PLpgSQLFragment::new(source, validation))
}

pub(super) fn compile_expression_source(source: &PLpgSQLSource) -> Result<Expr> {
    let query = &*source.query;
    let mode = source.mode;
    let parse_mode = mode.parser_mode();
    let node = parse_one_raw_node(query, parse_mode)?;
    match (mode, node.node.as_ref()) {
        (PLpgSQLParseMode::Expression, Some(pg_query::NodeEnum::SelectStmt(select))) => {
            compile_single_select_expression(select, query)
        }
        (
            PLpgSQLParseMode::Assignment1
            | PLpgSQLParseMode::Assignment2
            | PLpgSQLParseMode::Assignment3,
            Some(pg_query::NodeEnum::PlassignStmt(assign)),
        ) => {
            let expected_names = match mode {
                PLpgSQLParseMode::Assignment1 => 1,
                PLpgSQLParseMode::Assignment2 => 2,
                PLpgSQLParseMode::Assignment3 => 3,
                _ => unreachable!(),
            };
            if assign.nnames != expected_names {
                return Err(SQLError::Internal(format!(
                    "PL/pgSQL assignment parser returned {} target names for parse mode {mode:?}",
                    assign.nnames
                )));
            }
            let value = assign
                .val
                .as_deref()
                .ok_or_else(|| SQLError::Internal("PL/pgSQL assignment has no value".into()))?;
            compile_single_select_expression(value, query)
        }
        (_, Some(other)) => Err(SQLError::Internal(format!(
            "PL/pgSQL parse mode {mode:?} returned unexpected node {other:?}"
        ))),
        (_, None) => Err(SQLError::Internal(
            "PL/pgSQL expression parser returned an empty node".into(),
        )),
    }
}

/// Lower a `PLpgSQL_expr` node holding a complete SQL statement
/// (parse mode 0: queries, PERFORM bodies, CALL statements).
pub(super) fn lower_full_statement(
    raw: &JSONValue,
    mode: PLpgSQLCompileMode,
) -> Result<PLpgSQLStatement> {
    lower_sourced_statement(raw, mode).map(|(statement, _)| statement)
}

pub(super) fn lower_sourced_statement(
    raw: &JSONValue,
    mode: PLpgSQLCompileMode,
) -> Result<(PLpgSQLStatement, String)> {
    let source = source(raw)?;
    if source.mode != PLpgSQLParseMode::Statement {
        return Err(SQLError::Internal(
            "embedded PL/pgSQL statement has expression parse mode".into(),
        ));
    }
    let validation = mode
        .validates()
        .then(|| compile_statement_source(&source))
        .transpose()?;
    let query = source.query.to_string();
    Ok((PLpgSQLFragment::new(source, validation), query))
}

pub(super) fn compile_statement_source(source: &PLpgSQLSource) -> Result<Statement> {
    let mut statements = crate::compile(&source.query)?;
    match statements.len() {
        1 => Ok(statements.remove(0)),
        n => Err(SQLError::Internal(format!(
            "embedded PL/pgSQL query compiled to {n} statements"
        ))),
    }
}

pub(super) fn lower_cursor_arguments(
    raw: Option<&JSONValue>,
    mode: PLpgSQLCompileMode,
) -> Result<PLpgSQLCursorArguments> {
    let Some(raw) = raw else {
        return Ok(PLpgSQLCursorArguments::empty());
    };
    let source = source(raw)?;
    if source.mode != PLpgSQLParseMode::Expression {
        return Err(SQLError::Internal(
            "PL/pgSQL cursor arguments have non-expression parse mode".into(),
        ));
    }
    let validation = mode
        .validates()
        .then(|| compile_cursor_arguments_source(&source))
        .transpose()?;
    Ok(PLpgSQLFragment::new(source, validation))
}

pub(super) fn compile_cursor_arguments_source(
    source: &PLpgSQLSource,
) -> Result<Vec<PLpgSQLCursorArgument>> {
    let query = &*source.query;
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let node = parse_one_raw_node(query, pg_query::ParseMode::PlPgSqlExpr)?;
    let Some(pg_query::NodeEnum::SelectStmt(select)) = node.node.as_ref() else {
        return Err(SQLError::Internal(format!(
            "PL/pgSQL cursor arguments did not parse as a SELECT target list: {query}"
        )));
    };
    validate_select_expression_envelope(select, query)?;
    if select.target_list.is_empty() {
        return Err(SQLError::Parse(format!(
            "PL/pgSQL cursor argument list is empty: {query}"
        )));
    }
    Ok(
        crate::compiler::compile_pg_projections(&select.target_list)?
            .into_iter()
            .map(|projection| PLpgSQLCursorArgument {
                name: projection.alias.map(|name| name.to_ascii_lowercase()),
                expr: projection.expr,
            })
            .collect(),
    )
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
    let parsed = crate::parser::parse_with_mode(text, mode)?;
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
    if select.target_list.len() != 1 {
        return Err(SQLError::Parse(format!("not a single expression: {text}")));
    }
    if validate_select_expression_envelope(select, text).is_err() {
        return crate::compiler::compile_pg_select(select)
            .map(Box::new)
            .map(Expr::ScalarSubquery);
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

    #[test]
    fn expression_with_from_lowers_to_a_scalar_subquery() {
        let expression = compile_expression_text("max(a) FROM xacttest").unwrap();
        let Expr::ScalarSubquery(query) = expression else {
            panic!("PL/pgSQL query-shaped expression was not preserved as a scalar subquery");
        };
        assert_eq!(query.projections.len(), 1);
        assert!(query.from.is_some());
    }
}
