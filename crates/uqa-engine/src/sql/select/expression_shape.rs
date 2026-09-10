//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL-owned expression analysis used by execution planning.

pub(in crate::sql) use uqa_sql::semantics::{expr_has_unqualified_column, expr_qualifiers};
