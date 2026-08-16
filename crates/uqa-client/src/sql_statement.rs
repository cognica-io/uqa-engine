//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fmt;

use serde::Serialize;
use uqa_sql::SQLParam;

use crate::{sql_parameter::SQLParameter, HttpEngineError};

/// Owned SQL text and typed bind parameters for one HTTP operation.
#[derive(Serialize)]
pub struct SQLStatement {
    sql: String,
    params: Vec<SQLParameter>,
}

impl SQLStatement {
    pub fn new(sql: impl Into<String>, params: &[SQLParam]) -> Result<Self, HttpEngineError> {
        let sql = sql.into();
        if sql.trim().is_empty() {
            return Err(HttpEngineError::EmptySQL);
        }
        let params = params
            .iter()
            .map(SQLParameter::try_from)
            .collect::<Result<_, _>>()?;
        Ok(Self { sql, params })
    }
}

impl fmt::Debug for SQLStatement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SQLStatement")
            .field("sql", &"[REDACTED]")
            .field("parameter_count", &self.params.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_omits_sql() {
        let statement = SQLStatement::new("SELECT customer_secret", &[]).unwrap();
        assert!(!format!("{statement:?}").contains("customer_secret"));
    }
}
