//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::SQLDiagnosticCategory;

/// Bounded, statement-free SQL diagnostics shared by materialized and streamed errors.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "WireSQLDiagnostic", into = "WireSQLDiagnostic")]
pub struct SQLDiagnostic {
    sqlstate: Option<[u8; 5]>,
    pub category: SQLDiagnosticCategory,
    /// Zero-based index into the submitted batch; absent for transaction-boundary errors.
    pub statement_index: Option<u32>,
    /// One-based character position in the failing member; absent when unknown.
    pub position: Option<u32>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireSQLDiagnostic {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sqlstate: Option<String>,
    category: SQLDiagnosticCategory,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    statement_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    position: Option<u32>,
}

impl SQLDiagnostic {
    pub fn new(
        sqlstate: Option<&str>,
        category: SQLDiagnosticCategory,
        statement_index: Option<u32>,
        position: Option<u32>,
    ) -> Self {
        Self {
            sqlstate: sqlstate.and_then(valid_sqlstate),
            category,
            statement_index,
            position: position.filter(|value| *value != 0),
        }
    }

    pub fn sqlstate(&self) -> Option<&str> {
        self.sqlstate
            .as_ref()
            .and_then(|value| std::str::from_utf8(value).ok())
    }

    pub fn matches_code(&self, code: &str) -> bool {
        code == "SQL_EXECUTION_FAILED"
    }
}

impl TryFrom<WireSQLDiagnostic> for SQLDiagnostic {
    type Error = &'static str;

    fn try_from(wire: WireSQLDiagnostic) -> Result<Self, Self::Error> {
        let sqlstate = wire
            .sqlstate
            .as_deref()
            .map(|value| valid_sqlstate(value).ok_or("invalid SQLSTATE"))
            .transpose()?;
        if wire.position == Some(0) {
            return Err("invalid SQL position");
        }
        Ok(Self {
            sqlstate,
            category: wire.category,
            statement_index: wire.statement_index,
            position: wire.position,
        })
    }
}

impl From<SQLDiagnostic> for WireSQLDiagnostic {
    fn from(value: SQLDiagnostic) -> Self {
        Self {
            sqlstate: value.sqlstate().map(str::to_owned),
            category: value.category,
            statement_index: value.statement_index,
            position: value.position,
        }
    }
}

fn valid_sqlstate(value: &str) -> Option<[u8; 5]> {
    (value != "00000"
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit()))
    .then(|| value.as_bytes().try_into().ok())
    .flatten()
}

impl fmt::Display for SQLDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.category.as_str())?;
        if let Some(sqlstate) = self.sqlstate() {
            write!(formatter, "; SQLSTATE {sqlstate}")?;
        }
        if let Some(index) = self.statement_index {
            write!(
                formatter,
                "; batch statement {} (index {index})",
                u64::from(index) + 1
            )?;
        }
        if let Some(position) = self.position {
            write!(formatter, "; character {position}")?;
        }
        if let Some(hint) = self.category.hint() {
            write!(formatter, "; {hint}")?;
        }
        Ok(())
    }
}
