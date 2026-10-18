//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original procedural statement locations and `PostgreSQL` statement-kind names.

use super::{PLpgSQLParseMode, PLpgSQLSource, PLpgSQLStmt};

impl PLpgSQLSource {
    /// SPI identifies the original embedded input unless a parser position already identifies it.
    pub fn error_context(&self, error: crate::SQLError) -> crate::SQLError {
        if error.position().is_some_and(|position| position > 0) {
            return error;
        }
        let kind = match self.mode {
            PLpgSQLParseMode::Expression => "PL/pgSQL expression",
            PLpgSQLParseMode::Assignment1
            | PLpgSQLParseMode::Assignment2
            | PLpgSQLParseMode::Assignment3 => "PL/pgSQL assignment",
            PLpgSQLParseMode::Statement => "SQL statement",
        };
        error.with_context(format!("{kind} \"{}\"", self.query))
    }
}

impl PLpgSQLStmt {
    pub fn unlocated(&self) -> &Self {
        match self {
            Self::Located { statement, .. } => statement.unlocated(),
            _ => self,
        }
    }

    pub fn diagnostic_kind(&self) -> &'static str {
        match self.unlocated() {
            Self::Located { .. } => unreachable!("statement locations were removed"),
            Self::Block(_) => "statement block",
            Self::Assign { .. } => "assignment",
            Self::If { .. } => "IF",
            Self::Case { .. } => "CASE",
            Self::Loop { .. } => "LOOP",
            Self::While { .. } => "WHILE",
            Self::ForI { .. } => "FOR with integer loop variable",
            Self::ForQuery { .. } => "FOR over SELECT rows",
            Self::ForDynamic { .. } => "FOR over EXECUTE statement",
            Self::ForCursor { .. } => "FOR over cursor",
            Self::ForeachArray { .. } => "FOREACH over array",
            Self::Exit { is_exit: true, .. } => "EXIT",
            Self::Exit { is_exit: false, .. } => "CONTINUE",
            Self::Return { .. } => "RETURN",
            Self::ReturnNext { .. } => "RETURN NEXT",
            Self::ReturnQuery { .. } | Self::ReturnQueryExecute { .. } => "RETURN QUERY",
            Self::Raise { .. } => "RAISE",
            Self::Assert { .. } => "ASSERT",
            Self::ExecSQL { .. } => "SQL statement",
            Self::DynExecute { .. } => "EXECUTE",
            Self::Perform { .. } => "PERFORM",
            Self::OpenCursor { .. } => "OPEN",
            Self::FetchCursor { .. } => "FETCH",
            Self::MoveCursor { .. } => "MOVE",
            Self::CloseCursor { .. } => "CLOSE",
            Self::Commit { .. } => "COMMIT",
            Self::Rollback { .. } => "ROLLBACK",
            Self::GetDiagnostics { .. } => "GET DIAGNOSTICS",
        }
    }
}
