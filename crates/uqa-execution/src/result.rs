//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL result formatting, `PostgreSQL` type metadata and bounded columnar cursors.

pub use crate::query::cursor::{SQLCursor, SQLCursorSummary};
pub use uqa_sql::catalog::result_type::{postgres_result_type, SQLTypeMetadata};
pub use uqa_sql::result::format_postgres_text;
