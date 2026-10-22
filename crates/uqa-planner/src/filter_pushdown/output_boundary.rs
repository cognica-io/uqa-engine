//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Predicates may remove whole groups or partitions without changing their retained outputs.

use uqa_sql::{
    plan::{ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan},
    semantics::aggregates::exprs_match,
    ScalarExpr,
};

pub(super) fn preserves_set_types(
    plan: &QueryPlan,
    qualifier: &str,
    predicate: &ScalarExpr,
    aliases: Option<&[String]>,
) -> bool {
    if !matches!(plan.root, RelationalPlan::SetOp { .. }) {
        return true;
    }
    let Some(output) = plan.analyzed_output.as_ref() else {
        return false;
    };
    let mut positions = std::collections::BTreeSet::new();
    predicate.visit(&mut |node| {
        let column = match node {
            ScalarExpr::Column(column) => column,
            ScalarExpr::QualifiedColumn {
                qualifier: owner,
                column,
                ..
            } if owner == qualifier => column,
            _ => return,
        };
        if let Some(index) = output
            .columns()
            .iter()
            .enumerate()
            .position(|(index, name)| {
                aliases.and_then(|names| names.get(index)).unwrap_or(name) == column
            })
        {
            positions.insert(index);
        }
    });
    !positions.is_empty() && set_member_types_match(plan, output, &positions)
}

fn set_member_types_match(
    plan: &QueryPlan,
    output: &uqa_sql::RowSchema,
    positions: &std::collections::BTreeSet<usize>,
) -> bool {
    if let RelationalPlan::SetOp { left, right, .. } = &plan.root {
        return set_member_types_match(left, output, positions)
            && set_member_types_match(right, output, positions);
    }
    let Some(member) = plan.analyzed_output.as_ref() else {
        return false;
    };
    positions.iter().all(|index| {
        member
            .column_type(*index)
            .zip(output.column_type(*index))
            .is_some_and(|(input, output)| {
                uqa_sql::catalog::type_metadata::pg_type_oid(input)
                    == uqa_sql::catalog::type_metadata::pg_type_oid(output)
            })
    })
}

pub(super) fn preserves_groups(block: &QueryBlockPlan, expression: &ScalarExpr) -> bool {
    if (matches!(block.compute, ComputePlan::Aggregate) || !block.group_by.is_empty())
        && !block
            .group_by
            .iter()
            .any(|group| exprs_match(group, expression))
    {
        return false;
    }
    if !block.distinct_on.is_empty()
        && !block.distinct_on.iter().any(|key| {
            let key = match key {
                ScalarExpr::Position(index) => block
                    .projections
                    .get(*index)
                    .map(|p| &p.expr)
                    .unwrap_or(key),
                _ => key,
            };
            exprs_match(key, expression)
        })
    {
        return false;
    }
    if !matches!(block.compute, ComputePlan::Window) {
        return true;
    }
    let mut seen = false;
    let mut preserves = true;
    for root in block
        .projections
        .iter()
        .map(|projection| &projection.expr)
        .chain(block.order_by.iter().map(|order| &order.expr))
    {
        root.visit(&mut |node| {
            if let ScalarExpr::WindowCall { spec, .. } = node {
                seen = true;
                preserves &= spec
                    .partition_by
                    .iter()
                    .any(|key| exprs_match(key, expression));
            }
        });
    }
    seen && preserves
}
