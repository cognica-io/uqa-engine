//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;

use serde::Deserialize;
use uqa_core::Value;
use uqa_sql::SQLResult;

/// Materialized SQL result plus the data-plane request identity.
pub struct SQLExecution {
    result: SQLResult,
    request_id: String,
}

#[derive(Deserialize)]
pub(crate) struct SQLWireResponse {
    pub columns: Vec<String>,
    pub rows: Vec<BTreeMap<String, Value>>,
    pub affected_rows: u64,
    pub request_id: String,
}

impl SQLExecution {
    pub(crate) fn from_wire(response: SQLWireResponse) -> Self {
        let column_types = vec![None; response.columns.len()];
        Self {
            result: SQLResult {
                columns: response.columns,
                column_types,
                rows: response.rows,
                positional_rows: None,
                affected_rows: response.affected_rows,
            },
            request_id: response.request_id,
        }
    }

    pub fn result(&self) -> &SQLResult {
        &self.result
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn into_result(self) -> SQLResult {
        self.result
    }
}

impl Deref for SQLExecution {
    type Target = SQLResult;

    fn deref(&self) -> &Self::Target {
        &self.result
    }
}

impl fmt::Debug for SQLExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SQLExecution")
            .field("request_id", &self.request_id)
            .field("result", &"[REDACTED]")
            .finish()
    }
}
