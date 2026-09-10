//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

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
