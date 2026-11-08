//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fields, variant, Node, Part, SQLError};
use crate::plan::{
    AccessPathPlan, ComputePlan, CteCyclePlan, CtePlan, CtePlanBody, CteSearchPlan, ExpressionPlan,
    JoinExecutionStrategy, OrderPlan, ProjectionPlan, QueryBlockPlan, QueryPlan, RelationalPlan,
    TableFunctionPlan,
};

image_struct!(QueryPlan { relations_bound, ctes, root } skip {
    analyzed_output, analyzed_source, analyzed_grouping, relation_privilege_subject,
});
image_struct!(QueryBlockPlan {
    windows, projections, privilege_columns, from, r#where, compute,
    group_by, grouping_sets, group_distinct, having, order_by, limit,
    with_ties, offset, distinct, distinct_on, subqueries, access, locking,
} skip { deferred_outer_rewrites });
image_struct!(CtePlan {
    name,
    columns,
    recursive,
    materialization,
    search,
    cycle,
    body
});
image_struct!(CteSearchPlan {
    columns,
    breadth_first,
    sequence_column
});
image_struct!(CteCyclePlan {
    columns,
    mark_column,
    mark_value,
    mark_default,
    path_column
});
image_struct!(ProjectionPlan { expr, alias });
image_struct!(OrderPlan {
    expr,
    descending,
    nulls
});
image_struct!(ExpressionPlan { scalar, subqueries });
image_struct!(TableFunctionPlan {
    name,
    binding,
    output_name,
    relations,
    args,
    column_aliases,
    column_types,
});

image_enum!(RelationalPlan {
    0 => QueryBlock(value),
    1 => SetOp { kind, all, left, right, order_by, limit, with_ties, offset, subqueries },
    2 => Values { rows, subqueries },
});
image_enum!(CtePlanBody { 0 => Query(value), 1 => Command(value) });
image_enum!(AccessPathPlan { 0 => Row, 1 => OperatorTree { score_limit_pushdown }, 2 => Hybrid });
image_enum!(ComputePlan { 0 => Project, 1 => Aggregate, 2 => Window });
image_enum!(JoinExecutionStrategy { 0 => Auto, 1 => Hash });
