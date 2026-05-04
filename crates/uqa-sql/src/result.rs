//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Result rows returned by `Engine::sql`.

use std::collections::BTreeMap;

use uqa_core::Value;

pub type ResultRow = BTreeMap<String, Value>;

#[derive(Debug, Clone, Default)]
pub struct SqlResult {
    /// Column order as the SELECT clause specified.
    pub columns: Vec<String>,
    /// One row per result document, with the named columns in
    /// `columns`. Extra columns from `_score` etc. are included here
    /// too.
    pub rows: Vec<ResultRow>,
    /// Number of rows touched by an INSERT / UPDATE / DELETE.
    pub affected_rows: u64,
}

impl SqlResult {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_rows(columns: Vec<String>, rows: Vec<ResultRow>) -> Self {
        Self {
            columns,
            rows,
            affected_rows: 0,
        }
    }

    pub fn from_affected(affected: u64) -> Self {
        Self {
            affected_rows: affected,
            ..Self::default()
        }
    }
}
