//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

native_struct!(crate::ast::CTE {
    name,
    columns,
    recursive,
    materialization,
    search,
    cycle,
    body
});

native_enum!(recursive, crate::ast::CteBody {
    Query(value0),
    Insert(value0),
    Update(value0),
    Delete(value0),
    Merge(value0)
});

native_struct!(crate::ast::CteCycleClause {
    columns,
    mark_column,
    mark_value,
    mark_default,
    path_column
});

syntax_enum!(crate::ast::CteMaterialization {
    Default,
    Materialized,
    NotMaterialized
});

native_struct!(crate::ast::CteSearchClause {
    columns,
    breadth_first,
    sequence_column
});
