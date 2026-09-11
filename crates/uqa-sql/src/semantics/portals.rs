//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cursor declaration restrictions and command descriptor diagnostics.

#[cfg(test)]
mod tests;

use crate::{plan::CommandPlan, SQLError};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PortalDeclarationContext {
    Sql,
    PLpgSQL,
}

pub fn cannot_open_command_cursor(command: &CommandPlan) -> SQLError {
    let tag = match command {
        CommandPlan::Insert(_) => "INSERT",
        CommandPlan::Update(_) => "UPDATE",
        CommandPlan::Delete(_) => "DELETE",
        CommandPlan::Merge(_) => "MERGE",
        CommandPlan::Call { .. } => "CALL",
        CommandPlan::ShowVariable { .. } => "SHOW",
        CommandPlan::Explain { .. } => "EXPLAIN",
        _ => command.name(),
    };
    SQLError::Routine {
        sqlstate: "42P11".into(),
        message: format!("cannot open {tag} query as cursor"),
    }
}

pub fn validate_query_options(
    context: PortalDeclarationContext,
    has_row_locks: bool,
    hold: bool,
    scroll: Option<bool>,
) -> Result<(), SQLError> {
    if has_row_locks && hold {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "DECLARE CURSOR WITH HOLD ... FOR UPDATE is not supported".into(),
        });
    }
    if has_row_locks && scroll == Some(true) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: if context == PortalDeclarationContext::PLpgSQL {
                "DECLARE SCROLL CURSOR ... FOR UPDATE/SHARE is not supported".into()
            } else {
                "DECLARE SCROLL CURSOR ... FOR UPDATE is not supported".into()
            },
        });
    }
    Ok(())
}

pub fn command_scroll_returns_nulls(
    command: &CommandPlan,
    scroll: Option<bool>,
) -> Result<bool, SQLError> {
    if scroll == Some(true) && matches!(command, CommandPlan::Merge(_)) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "DECLARE SCROLL CURSOR ... FOR UPDATE/SHARE is not supported".into(),
        });
    }
    let null_returning_values = scroll == Some(true)
        && matches!(
            command,
            CommandPlan::Insert(_) | CommandPlan::Update(_) | CommandPlan::Delete(_)
        );
    Ok(null_returning_values)
}
