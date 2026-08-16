//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use uqa_core::Value;
use uqa_sql::SQLResult;

/// Results from one atomically executed HTTP SQL batch.
pub struct SQLBatchExecution {
    results: Vec<SQLResult>,
    request_id: String,
}

#[derive(Deserialize)]
pub(crate) struct SQLBatchWireResponse {
    results: Vec<SQLStatementWireResponse>,
    pub request_id: String,
}

#[derive(Deserialize)]
struct SQLStatementWireResponse {
    columns: Vec<String>,
    rows: Vec<BTreeMap<String, Value>>,
    affected_rows: u64,
}

impl SQLBatchExecution {
    pub(crate) fn from_wire(response: SQLBatchWireResponse) -> Self {
        let results = response
            .results
            .into_iter()
            .map(|result| {
                let column_types = vec![None; result.columns.len()];
                SQLResult {
                    columns: result.columns,
                    column_types,
                    rows: result.rows,
                    positional_rows: None,
                    affected_rows: result.affected_rows,
                }
            })
            .collect();
        Self {
            results,
            request_id: response.request_id,
        }
    }

    pub fn results(&self) -> &[SQLResult] {
        &self.results
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn into_results(self) -> Vec<SQLResult> {
        self.results
    }
}

impl fmt::Debug for SQLBatchExecution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SQLBatchExecution")
            .field("request_id", &self.request_id)
            .field("results", &"[REDACTED]")
            .finish()
    }
}
