//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fields, variant, Node, Part, SQLError};
use crate::ast::{CteBody, CteCycleClause, CteSearchClause, SelectStmt, SetOp, CTE};

image_struct!(SelectStmt {
    windows,
    projections,
    values,
    from,
    r#where,
    group_by,
    grouping_sets,
    group_distinct,
    having,
    order_by,
    limit,
    with_ties,
    offset,
    with,
    set_op,
    distinct,
    distinct_on,
    locking,
});
image_struct!(SetOp {
    kind,
    all,
    left,
    right,
    combined_order_by,
    combined_limit,
    combined_with_ties,
    combined_offset,
});
image_struct!(CTE {
    name,
    columns,
    recursive,
    materialization,
    search,
    cycle,
    body
});
image_enum!(CteBody {
    0 => Query(value), 1 => Insert(value), 2 => Update(value), 3 => Delete(value), 4 => Merge(value),
});
image_struct!(CteSearchClause {
    columns,
    breadth_first,
    sequence_column
});
image_struct!(CteCycleClause {
    columns,
    mark_column,
    mark_value,
    mark_default,
    path_column
});
