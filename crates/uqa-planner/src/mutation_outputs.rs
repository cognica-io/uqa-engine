//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prune unused, side-effect-free mutation-source output slots without changing row shape.
use std::collections::BTreeSet;
use uqa_core::Value;
use uqa_sql::{
    plan::{ComputePlan, QueryPlan, RelationalPlan},
    ScalarExpr,
};

pub fn prune_unused_query_outputs(
    query: &mut QueryPlan,
    required_positions: &BTreeSet<usize>,
    expected_width: usize,
) {
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        return;
    };
    let projection_can_be_pruned = matches!(block.compute, ComputePlan::Project)
        && !block.distinct
        && block.distinct_on.is_empty()
        && block.order_by.is_empty()
        && block.projections.len() == expected_width;
    if !projection_can_be_pruned {
        return;
    }
    for (position, projection) in block.projections.iter_mut().enumerate() {
        if !required_positions.contains(&position) {
            projection.expr = ScalarExpr::Literal(Value::Null);
        }
    }
}
