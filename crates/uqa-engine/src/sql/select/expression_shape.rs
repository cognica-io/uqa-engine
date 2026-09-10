//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL-owned expression analysis used by execution planning.

pub(in crate::sql) use uqa_sql::semantics::{
    expr_contains_function, expr_has_unqualified_column, expr_qualifiers, flatten_and_filter_parts,
    from_qualifier_set, qualify_unqualified_columns,
};
