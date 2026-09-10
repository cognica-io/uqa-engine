//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL-owned plan types retained at the planner's public API boundary.

pub(crate) use uqa_sql::ast::is_builtin_aggregate_function as is_builtin_aggregate;
pub use uqa_sql::plan::*;
