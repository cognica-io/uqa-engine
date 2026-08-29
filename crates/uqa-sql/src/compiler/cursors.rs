//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL cursor statement lowering.

use super::{compile_select, NodeEnum, Result, SQLError, Statement};
use crate::ast::{CursorDirection, DeclareCursorStmt, FetchCursorStmt};

const CURSOR_OPT_BINARY: i32 = 0x0001;
const CURSOR_OPT_SCROLL: i32 = 0x0002;
const CURSOR_OPT_NO_SCROLL: i32 = 0x0004;
const CURSOR_OPT_HOLD: i32 = 0x0020;

pub(super) fn compile_declare_cursor(
    stmt: &pg_query::protobuf::DeclareCursorStmt,
) -> Result<Statement> {
    let query = stmt
        .query
        .as_deref()
        .and_then(|node| node.node.as_ref())
        .ok_or_else(|| SQLError::Internal("DECLARE CURSOR has no query".into()))?;
    let NodeEnum::SelectStmt(query) = query else {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "DECLARE CURSOR query is not a SELECT".into(),
        });
    };
    let scroll = if stmt.options & CURSOR_OPT_SCROLL != 0 {
        Some(true)
    } else if stmt.options & CURSOR_OPT_NO_SCROLL != 0 {
        Some(false)
    } else {
        None
    };
    Ok(Statement::DeclareCursor(DeclareCursorStmt {
        name: stmt.portalname.clone(),
        binary: stmt.options & CURSOR_OPT_BINARY != 0,
        scroll,
        hold: stmt.options & CURSOR_OPT_HOLD != 0,
        query: Box::new(compile_select(query)?),
    }))
}

pub(super) fn compile_fetch_cursor(stmt: &pg_query::protobuf::FetchStmt) -> Result<Statement> {
    use pg_query::protobuf::FetchDirection;

    let direction = match stmt.direction() {
        FetchDirection::FetchForward => CursorDirection::Forward,
        FetchDirection::FetchBackward => CursorDirection::Backward,
        FetchDirection::FetchAbsolute => CursorDirection::Absolute,
        FetchDirection::FetchRelative => CursorDirection::Relative,
        FetchDirection::Undefined => {
            return Err(SQLError::Internal(
                "FETCH parser returned an undefined direction".into(),
            ));
        }
    };
    Ok(Statement::FetchCursor(FetchCursorStmt {
        name: stmt.portalname.clone(),
        direction,
        count: stmt.how_many,
        move_only: stmt.ismove,
    }))
}

pub(super) fn compile_close_cursor(stmt: &pg_query::protobuf::ClosePortalStmt) -> Statement {
    Statement::CloseCursor {
        name: (!stmt.portalname.is_empty()).then(|| stmt.portalname.clone()),
    }
}
