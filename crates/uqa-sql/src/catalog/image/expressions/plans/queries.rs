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

projected_struct!(recursive, QueryPlan { relations_bound, ctes, root } skip {
    analyzed_output, analyzed_source, analyzed_grouping, relation_privilege_subject,
});
projected_struct!(recursive, QueryBlockPlan {
    windows, projections, privilege_columns, from, r#where, compute,
    group_by, grouping_sets, group_distinct, having, order_by, limit,
    with_ties, offset, distinct, distinct_on, subqueries, access, locking,
} skip { deferred_outer_rewrites } prefixed { privilege_subqueries });
projected_struct!(
    inline,
    CtePlan {
        name,
        columns,
        recursive,
        materialization,
        search,
        cycle,
        body
    }
);
projected_struct!(
    inline,
    CteSearchPlan {
        columns,
        breadth_first,
        sequence_column
    }
);
projected_struct!(
    inline,
    CteCyclePlan {
        columns,
        mark_column,
        mark_value,
        mark_default,
        path_column
    }
);
projected_struct!(inline, ProjectionPlan { expr, alias });
projected_struct!(
    inline,
    OrderPlan {
        expr,
        descending,
        nulls
    }
);
image_struct!(ExpressionPlan { scalar, subqueries });
projected_struct!(
    inline,
    TableFunctionPlan {
        name,
        binding,
        output_name,
        relations,
        args,
        column_aliases,
        column_types,
    }
);

// Tag 0 keeps the original query-block framing. Tag 3 prefixes the same block with its retained privilege slots.
projected_enum!(inline, RelationalPlan {
    0 => QueryBlock(value),
    1 => SetOp { kind, all, left, right, order_by, limit, with_ties, offset, subqueries },
    2 => Values { rows, subqueries },
} extended {
    Self::QueryBlock(block) if !block.privilege_subqueries.is_empty() => 3, [&block.privilege_subqueries, block]
});
projected_enum!(inline, CtePlanBody { 0 => Query(value), 1 => Command(value) });
projected_enum!(inline, AccessPathPlan { 0 => Row, 1 => OperatorTree { score_limit_pushdown }, 2 => Hybrid });
projected_enum!(inline, ComputePlan { 0 => Project, 1 => Aggregate, 2 => Window });
projected_enum!(inline, JoinExecutionStrategy { 0 => Auto, 1 => Hash, 2 => IndexNestedLoop { inner_column } });
