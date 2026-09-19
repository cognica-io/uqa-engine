//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

/// A cursor's declaration metadata, independent of its executable and retained rows.
#[derive(Clone)]
pub struct CursorMetadata {
    pub name: String,
    pub source_sql: Option<Arc<str>>,
    pub is_holdable: bool,
    pub is_binary: bool,
    pub is_scrollable: bool,
    pub created_at_micros: i64,
}

/// Session catalog data without executable plans or planner ownership.
pub struct PreparedStatementMetadata {
    pub name: String,
    pub parameter_types: Vec<Option<crate::ColumnType>>,
    pub result_types: Option<Vec<Option<crate::ColumnType>>>,
    pub source_sql: Option<Arc<str>>,
    pub prepared_at_micros: i64,
    pub from_sql: bool,
    pub generic_plans: i64,
    pub custom_plans: i64,
}
