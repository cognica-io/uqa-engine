//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Block, statement, INTO-target, and statement-list lowering.

use super::lowering_expression::lower_cursor_arguments;
use super::{
    ensure_single_tag, expect_tag, json_bool_or_false, json_kind, json_optional_i64,
    json_optional_str, json_optional_usize, json_usize_or_zero, lower_expr, lower_expr_list,
    lower_full_statement, lower_row_fields, normalize_condition, optional_array, require,
    require_i64, require_nonempty_str, validate_assignable_datum, validate_record_datum,
    validate_scalar_datum, CursorDirection, IntoTarget, JSONValue, PLpgSQLBlock,
    PLpgSQLCursorCount, PLpgSQLCursorOpen, PLpgSQLDatum, PLpgSQLExceptionArm, PLpgSQLReturnValue,
    PLpgSQLStmt, RaiseLevel, Result, SQLError,
};

pub(super) fn lower_block(block: &JSONValue, datums: &[PLpgSQLDatum]) -> Result<PLpgSQLBlock> {
    let body = lower_optional_stmt_list(block, "body", datums)?;
    let mut exceptions = Vec::new();
    if let Some(raw_exceptions) = block.get("exceptions") {
        let exc = expect_tag(raw_exceptions, "PLpgSQL_exception_block", "exception block")?;
        let list = optional_array(exc, "exc_list")?.ok_or_else(|| {
            SQLError::Internal("PL/pgSQL exception block without an arm list".into())
        })?;
        if list.is_empty() {
            return Err(SQLError::Internal(
                "PL/pgSQL exception block without arms".into(),
            ));
        }
        for arm in list {
            let arm = expect_tag(arm, "PLpgSQL_exception", "exception arm")?;
            let mut conditions = Vec::new();
            if let Some(conds) = optional_array(arm, "conditions")? {
                for cond in conds {
                    let cond = expect_tag(cond, "PLpgSQL_condition", "exception condition")?;
                    let name = json_optional_str(cond, "condname")?;
                    let state = json_optional_str(cond, "sqlstate")?;
                    let value = match (name, state) {
                        (Some(name), None) if !name.is_empty() => name,
                        (None, Some(state)) if !state.is_empty() => state,
                        _ => {
                            return Err(SQLError::Internal(
                                    "PL/pgSQL exception condition must have exactly one non-empty condition name or SQLSTATE"
                                        .into(),
                                ));
                        }
                    };
                    conditions.push(normalize_condition(value, true)?);
                }
            }
            if conditions.is_empty() {
                return Err(SQLError::Internal(
                    "PL/pgSQL exception arm without conditions".into(),
                ));
            }
            let body = lower_optional_stmt_list(arm, "action", datums)?;
            exceptions.push(PLpgSQLExceptionArm { conditions, body });
        }
    }
    Ok(PLpgSQLBlock {
        label: json_optional_str(block, "label")?,
        body,
        exceptions,
    })
}

pub(super) fn lower_stmt_list(
    list: &JSONValue,
    datums: &[PLpgSQLDatum],
) -> Result<Vec<PLpgSQLStmt>> {
    let items = list
        .as_array()
        .ok_or_else(|| SQLError::Internal("PL/pgSQL statement list is not an array".into()))?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(lower_stmt(item, datums)?);
    }
    Ok(out)
}

pub(super) fn lower_optional_stmt_list(
    object: &JSONValue,
    key: &str,
    datums: &[PLpgSQLDatum],
) -> Result<Vec<PLpgSQLStmt>> {
    match object.get(key) {
        Some(list) => lower_stmt_list(list, datums),
        None => Ok(Vec::new()),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "PL/pgSQL lowering preserves parser order and datum validation"
)]
pub(super) fn lower_stmt(raw: &JSONValue, datums: &[PLpgSQLDatum]) -> Result<PLpgSQLStmt> {
    ensure_single_tag(raw, "statement")?;
    if let Some(block) = raw.get("PLpgSQL_stmt_block") {
        return Ok(PLpgSQLStmt::Block(lower_block(block, datums)?));
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_assign") {
        // Zero-valued varno fields are omitted from the JSON dump.
        let target = json_usize_or_zero(stmt, "varno")?;
        validate_assignable_datum(datums, target, "assignment target")?;
        let expr =
            lower_expr(stmt.get("expr").ok_or_else(|| {
                SQLError::Internal("PL/pgSQL assignment without expression".into())
            })?)?;
        return Ok(PLpgSQLStmt::Assign { target, expr });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_if") {
        let cond = lower_expr(require(stmt, "cond")?)?;
        let then_body = lower_optional_stmt_list(stmt, "then_body", datums)?;
        let mut elsifs = Vec::new();
        if let Some(list) = optional_array(stmt, "elsif_list")? {
            for e in list {
                let e = expect_tag(e, "PLpgSQL_if_elsif", "ELSIF arm")?;
                let cond = lower_expr(require(e, "cond")?)?;
                let body = lower_optional_stmt_list(e, "stmts", datums)?;
                elsifs.push((cond, body));
            }
        }
        let else_body = match stmt.get("else_body") {
            Some(body) => Some(lower_stmt_list(body, datums)?),
            None => None,
        };
        return Ok(PLpgSQLStmt::If {
            cond,
            then_body,
            elsifs,
            else_body,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_case") {
        let t_expr = match stmt.get("t_expr") {
            Some(node) => Some(lower_expr(node)?),
            None => None,
        };
        let t_varno = if t_expr.is_some() {
            let index = json_usize_or_zero(stmt, "t_varno")?;
            validate_scalar_datum(datums, index, "CASE temporary")?;
            Some(index)
        } else {
            if json_optional_usize(stmt, "t_varno")?.is_some() {
                return Err(SQLError::Internal(
                    "PL/pgSQL searched CASE has a temporary datum but no expression".into(),
                ));
            }
            None
        };
        let mut arms = Vec::new();
        if let Some(list) = optional_array(stmt, "case_when_list")? {
            for arm in list {
                let arm = expect_tag(arm, "PLpgSQL_case_when", "CASE arm")?;
                let cond = lower_expr(require(arm, "expr")?)?;
                let body = lower_optional_stmt_list(arm, "stmts", datums)?;
                arms.push((cond, body));
            }
        }
        if arms.is_empty() {
            return Err(SQLError::Internal("PL/pgSQL CASE without arms".into()));
        }
        let have_else = json_bool_or_false(stmt, "have_else")?;
        let else_body = if have_else {
            Some(lower_optional_stmt_list(stmt, "else_stmts", datums)?)
        } else {
            if stmt.get("else_stmts").is_some() {
                return Err(SQLError::Internal(
                    "PL/pgSQL CASE has ELSE statements while have_else is false".into(),
                ));
            }
            None
        };
        return Ok(PLpgSQLStmt::Case {
            t_expr,
            t_varno,
            arms,
            else_body,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_loop") {
        return Ok(PLpgSQLStmt::Loop {
            label: json_optional_str(stmt, "label")?,
            body: lower_optional_stmt_list(stmt, "body", datums)?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_while") {
        return Ok(PLpgSQLStmt::While {
            label: json_optional_str(stmt, "label")?,
            cond: lower_expr(require(stmt, "cond")?)?,
            body: lower_optional_stmt_list(stmt, "body", datums)?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_fori") {
        let var_obj = require(stmt, "var")?
            .get("PLpgSQL_var")
            .ok_or_else(|| SQLError::Internal("FOR loop variable is not a var".into()))?;
        let name = require_nonempty_str(var_obj, "refname", "FOR loop variable")?;
        let lineno = json_optional_i64(var_obj, "lineno")?;
        let var = find_var_datum(datums, &name, lineno).ok_or_else(|| {
            SQLError::Internal(format!("FOR loop variable `{name}` has no datum"))
        })?;
        let step = match stmt.get("step") {
            Some(node) => Some(lower_expr(node)?),
            None => None,
        };
        return Ok(PLpgSQLStmt::ForI {
            label: json_optional_str(stmt, "label")?,
            var,
            lower: lower_expr(require(stmt, "lower")?)?,
            upper: lower_expr(require(stmt, "upper")?)?,
            step,
            reverse: json_bool_or_false(stmt, "reverse")?,
            body: lower_optional_stmt_list(stmt, "body", datums)?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_fors") {
        let target = lower_into_target(require(stmt, "var")?, datums)?;
        let query = lower_full_statement(require(stmt, "query")?)?;
        return Ok(PLpgSQLStmt::ForQuery {
            label: json_optional_str(stmt, "label")?,
            target,
            query,
            body: lower_optional_stmt_list(stmt, "body", datums)?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_exit") {
        let cond = match stmt.get("cond") {
            Some(node) => Some(lower_expr(node)?),
            None => None,
        };
        return Ok(PLpgSQLStmt::Exit {
            is_exit: json_bool_or_false(stmt, "is_exit")?,
            label: json_optional_str(stmt, "label")?,
            cond,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_return") {
        return Ok(PLpgSQLStmt::Return {
            value: lower_return_value(stmt, datums, "RETURN")?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_return_next") {
        return Ok(PLpgSQLStmt::ReturnNext {
            value: lower_return_value(stmt, datums, "RETURN NEXT")?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_return_query") {
        if let Some(query) = stmt.get("query") {
            return Ok(PLpgSQLStmt::ReturnQuery {
                query: lower_full_statement(query)?,
            });
        }
        if let Some(dynquery) = stmt.get("dynquery") {
            return Ok(PLpgSQLStmt::ReturnQueryExecute {
                query: lower_expr(dynquery)?,
                params: lower_expr_list(stmt.get("params"))?,
            });
        }
        return Err(SQLError::Internal("RETURN QUERY without a query".into()));
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_raise") {
        if stmt.get("options").is_some() {
            return Err(SQLError::Unsupported("RAISE ... USING options".into()));
        }
        let raw_level = require_i64(stmt, "elog_level", "RAISE statement")?;
        let level = match raw_level {
            10..=14 => RaiseLevel::Debug,
            15 | 16 => RaiseLevel::Log,
            17 => RaiseLevel::Info,
            18 => RaiseLevel::Notice,
            19 | 20 => RaiseLevel::Warning,
            21 => RaiseLevel::Error,
            other => {
                return Err(SQLError::Internal(format!(
                    "PL/pgSQL RAISE has invalid elog level {other}"
                )));
            }
        };
        let condition = json_optional_str(stmt, "condname")?
            .map(|condition| normalize_condition(condition, false))
            .transpose()?;
        return Ok(PLpgSQLStmt::Raise {
            level,
            condition,
            message: json_optional_str(stmt, "message")?,
            params: lower_expr_list(stmt.get("params"))?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_execsql") {
        let sql = lower_full_statement(require(stmt, "sqlstmt")?)?;
        let has_into = json_bool_or_false(stmt, "into")?;
        let strict = json_bool_or_false(stmt, "strict")?;
        if strict && !has_into {
            return Err(SQLError::Internal(
                "PL/pgSQL SQL statement is STRICT without INTO".into(),
            ));
        }
        let into = if has_into {
            Some(lower_into_target(
                require(stmt, "target").map_err(|_| {
                    SQLError::Internal("PL/pgSQL SQL statement has INTO but no target".into())
                })?,
                datums,
            )?)
        } else {
            if stmt.get("target").is_some() {
                return Err(SQLError::Internal(
                    "PL/pgSQL SQL statement has a target but INTO is false".into(),
                ));
            }
            None
        };
        return Ok(PLpgSQLStmt::ExecSQL {
            stmt: sql,
            into,
            strict,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_dynexecute") {
        let query = lower_expr(require(stmt, "query")?)?;
        let has_into = json_bool_or_false(stmt, "into")?;
        let strict = json_bool_or_false(stmt, "strict")?;
        if strict && !has_into {
            return Err(SQLError::Internal(
                "PL/pgSQL EXECUTE is STRICT without INTO".into(),
            ));
        }
        let into = if has_into {
            Some(lower_into_target(
                require(stmt, "target").map_err(|_| {
                    SQLError::Internal("PL/pgSQL EXECUTE has INTO but no target".into())
                })?,
                datums,
            )?)
        } else {
            if stmt.get("target").is_some() {
                return Err(SQLError::Internal(
                    "PL/pgSQL EXECUTE has a target but INTO is false".into(),
                ));
            }
            None
        };
        return Ok(PLpgSQLStmt::DynExecute {
            query,
            params: lower_expr_list(stmt.get("params"))?,
            into,
            strict,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_perform") {
        return Ok(PLpgSQLStmt::Perform {
            query: lower_full_statement(require(stmt, "expr")?)?,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_open") {
        let cursor = json_usize_or_zero(stmt, "curvar")?;
        validate_cursor_datum(datums, cursor, "OPEN")?;
        let bound = matches!(
            datums.get(cursor),
            Some(PLpgSQLDatum::Var(variable)) if variable.cursor.is_some()
        );
        let query = stmt.get("query");
        let dynquery = stmt.get("dynquery");
        let argquery = stmt.get("argquery");
        let forms = usize::from(query.is_some())
            + usize::from(dynquery.is_some())
            + usize::from(argquery.is_some());
        if forms > 1 {
            return Err(SQLError::Internal(
                "PL/pgSQL OPEN contains multiple query forms".into(),
            ));
        }
        let open = if let Some(query) = query {
            if bound {
                return Err(SQLError::Internal(
                    "PL/pgSQL bound cursor OPEN contains a static query".into(),
                ));
            }
            PLpgSQLCursorOpen::Static {
                query: Box::new(lower_full_statement(query)?),
                scroll: lower_cursor_scroll_options(stmt, "OPEN")?,
            }
        } else if let Some(query) = dynquery {
            if bound {
                return Err(SQLError::Internal(
                    "PL/pgSQL bound cursor OPEN contains a dynamic query".into(),
                ));
            }
            PLpgSQLCursorOpen::Dynamic {
                query: lower_expr(query)?,
                params: lower_expr_list(stmt.get("params"))?,
                scroll: lower_cursor_scroll_options(stmt, "OPEN")?,
            }
        } else {
            if !bound {
                return Err(SQLError::Unsupported(
                    "PL/pgSQL OPEN without FOR requires a bound cursor".into(),
                ));
            }
            PLpgSQLCursorOpen::Bound {
                arguments: lower_cursor_arguments(argquery)?,
            }
        };
        return Ok(PLpgSQLStmt::OpenCursor { cursor, open });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_fetch") {
        let cursor = json_usize_or_zero(stmt, "curvar")?;
        let is_move = json_bool_or_false(stmt, "is_move")?;
        validate_cursor_datum(datums, cursor, if is_move { "MOVE" } else { "FETCH" })?;
        let direction = lower_cursor_direction(require_i64(
            stmt,
            "direction",
            if is_move {
                "MOVE direction"
            } else {
                "FETCH direction"
            },
        )?)?;
        let count = match stmt.get("expr") {
            Some(expr) => PLpgSQLCursorCount::Expression(lower_expr(expr)?),
            None => PLpgSQLCursorCount::Constant(require_i64(
                stmt,
                "how_many",
                if is_move { "MOVE count" } else { "FETCH count" },
            )?),
        };
        if is_move {
            if stmt.get("target").is_some() {
                return Err(SQLError::Internal(
                    "PL/pgSQL MOVE unexpectedly contains a target".into(),
                ));
            }
            return Ok(PLpgSQLStmt::MoveCursor {
                cursor,
                direction,
                count,
            });
        }
        return Ok(PLpgSQLStmt::FetchCursor {
            cursor,
            target: lower_into_target(require(stmt, "target")?, datums)?,
            direction,
            count,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_close") {
        let cursor = json_usize_or_zero(stmt, "curvar")?;
        validate_cursor_datum(datums, cursor, "CLOSE")?;
        return Ok(PLpgSQLStmt::CloseCursor { cursor });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_call") {
        // CALL inside a body: run the CALL statement; INOUT results
        // flow back through the target row like an INTO clause.
        let call = lower_full_statement(require(stmt, "expr")?)?;
        let into = match stmt.get("target") {
            Some(target) => Some(lower_into_target(target, datums)?),
            None => None,
        };
        return Ok(PLpgSQLStmt::ExecSQL {
            stmt: call,
            into,
            strict: false,
        });
    }
    if let Some(stmt) = raw.get("PLpgSQL_stmt_getdiag") {
        if json_bool_or_false(stmt, "is_stacked")? {
            return Err(SQLError::Unsupported("GET STACKED DIAGNOSTICS".into()));
        }
        let mut items = Vec::new();
        if let Some(list) = optional_array(stmt, "diag_items")? {
            for item in list {
                let item = expect_tag(item, "PLpgSQL_diag_item", "diagnostics item")?;
                let kind =
                    require_nonempty_str(item, "kind", "diagnostics item")?.to_ascii_uppercase();
                let target = json_usize_or_zero(item, "target")?;
                validate_scalar_datum(datums, target, "diagnostics target")?;
                items.push((kind, target));
            }
        }
        if items.is_empty() {
            return Err(SQLError::Internal(
                "PL/pgSQL GET DIAGNOSTICS without items".into(),
            ));
        }
        return Ok(PLpgSQLStmt::GetDiagnostics { items });
    }
    Err(SQLError::Unsupported(format!(
        "PL/pgSQL statement {}",
        json_kind(raw)
    )))
}

fn lower_return_value(
    stmt: &JSONValue,
    datums: &[PLpgSQLDatum],
    context: &str,
) -> Result<Option<PLpgSQLReturnValue>> {
    let expr = stmt.get("expr");
    let datum = json_optional_usize(stmt, "retvarno")?;
    match (expr, datum) {
        (Some(_), Some(_)) => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} contains both expr and retvarno"
        ))),
        (Some(expr), None) => Ok(Some(PLpgSQLReturnValue::Expr(lower_expr(expr)?))),
        (None, Some(index)) => {
            match datums.get(index) {
                Some(
                    PLpgSQLDatum::Var(_) | PLpgSQLDatum::Rec { .. } | PLpgSQLDatum::Row { .. },
                ) => {}
                Some(_) => {
                    return Err(SQLError::Internal(format!(
                        "PL/pgSQL {context} retvarno {index} is not a returnable datum"
                    )));
                }
                None => {
                    return Err(SQLError::Internal(format!(
                        "PL/pgSQL {context} references missing retvarno datum {index}"
                    )));
                }
            }
            Ok(Some(PLpgSQLReturnValue::Datum(index)))
        }
        (None, None) => Ok(None),
    }
}

fn validate_cursor_datum(datums: &[PLpgSQLDatum], index: usize, context: &str) -> Result<()> {
    match datums.get(index) {
        Some(PLpgSQLDatum::Var(variable)) if variable.type_name == "refcursor" => Ok(()),
        Some(_) => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} datum {index} is not a refcursor"
        ))),
        None => Err(SQLError::Internal(format!(
            "PL/pgSQL {context} references missing cursor datum {index}"
        ))),
    }
}

const CURSOR_OPT_SCROLL: i64 = 0x0002;
const CURSOR_OPT_NO_SCROLL: i64 = 0x0004;
const CURSOR_OPT_FAST_PLAN: i64 = 0x0100;

pub(super) fn lower_cursor_scroll_options(node: &JSONValue, context: &str) -> Result<Option<bool>> {
    let options = require_i64(node, "cursor_options", &format!("{context} cursor options"))?;
    let unknown = options & !(CURSOR_OPT_SCROLL | CURSOR_OPT_NO_SCROLL | CURSOR_OPT_FAST_PLAN);
    if unknown != 0 {
        return Err(SQLError::Internal(format!(
            "PL/pgSQL {context} has unknown cursor options 0x{unknown:x}"
        )));
    }
    match (
        options & CURSOR_OPT_SCROLL != 0,
        options & CURSOR_OPT_NO_SCROLL != 0,
    ) {
        (true, false) => Ok(Some(true)),
        (false, true) => Ok(Some(false)),
        (false, false) => Ok(None),
        (true, true) => Err(SQLError::Internal(
            "PL/pgSQL {context} is both SCROLL and NO SCROLL".into(),
        )),
    }
}

fn lower_cursor_direction(direction: i64) -> Result<CursorDirection> {
    match direction {
        0 => Ok(CursorDirection::Forward),
        1 => Ok(CursorDirection::Backward),
        2 => Ok(CursorDirection::Absolute),
        3 => Ok(CursorDirection::Relative),
        other => Err(SQLError::Internal(format!(
            "PL/pgSQL cursor has invalid direction {other}"
        ))),
    }
}

/// Resolve a loop variable to its datum index. The JSON dump omits
/// `dno` on embedded vars, so match by name + declaration line, then
/// fall back to the last datum with that name.
pub(super) fn find_var_datum(
    datums: &[PLpgSQLDatum],
    name: &str,
    lineno: Option<i64>,
) -> Option<usize> {
    if lineno.is_some() {
        for (idx, d) in datums.iter().enumerate() {
            if let PLpgSQLDatum::Var(v) = d {
                if v.name == name && v.lineno == lineno {
                    return Some(idx);
                }
            }
        }
    }
    datums
        .iter()
        .rposition(|d| matches!(d, PLpgSQLDatum::Var(v) if v.name == name))
}

pub(super) fn lower_into_target(raw: &JSONValue, datums: &[PLpgSQLDatum]) -> Result<IntoTarget> {
    ensure_single_tag(raw, "INTO target")?;
    if let Some(rec) = raw.get("PLpgSQL_rec") {
        // Zero-valued dno fields are omitted from the JSON dump.
        let index = json_usize_or_zero(rec, "dno")?;
        validate_record_datum(datums, index, "INTO record target")?;
        return Ok(IntoTarget::Rec(index));
    }
    if let Some(row) = raw.get("PLpgSQL_row") {
        let fields = lower_row_fields(row)?;
        for field in &fields {
            validate_assignable_datum(datums, field.varno, "INTO row target")?;
        }
        if fields.is_empty() {
            return Err(SQLError::Internal(
                "PL/pgSQL INTO row target has no fields".into(),
            ));
        }
        return Ok(IntoTarget::Row(fields));
    }
    Err(SQLError::Unsupported(format!(
        "PL/pgSQL INTO target {}",
        json_kind(raw)
    )))
}
